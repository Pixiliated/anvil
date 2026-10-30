//! Name resolution and type checking: AST in, typed IR out. Collects every error it can find
//! rather than stopping at the first, and never lets one error cause another (Ty::Error).

use crate::ast::{self, BinOp, Else, ExprKind as AK, InterpPart, StmtKind, TypeKind, UnOp};
use crate::diag::{self, Diagnostic, Edit, FixKind};
use crate::ir::*;
use crate::span::{SourceFile, Span};
use crate::units::Unit;
use std::collections::{HashMap, HashSet};

const BUILTINS: &[&str] = &["print", "assert", "sqrt", "abs", "to_f64", "to_i64"];
/// Constructors of the built-in Option and Result enums, usable without a type prefix.
const CONSTRUCTORS: &[&str] = &["Some", "None", "Ok", "Err"];
const TYPE_NAMES: &[&str] = &["i64", "f64", "bool", "str", "Option", "Result"];
const INDENT: &str = "    ";

fn printable(ty: &Ty) -> bool {
    matches!(ty, Ty::I64 | Ty::F64(_) | Ty::Bool | Ty::Str)
}

struct FuncSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
    uses_io: bool,
    span: Span,
}

/// A declared name: functions, types, fields, variants, units, parameters and locals.
#[derive(Clone, Debug)]
pub struct Def {
    /// The span of the name where it is declared; uses point here.
    pub span: Span,
    /// `area`, `Shape`, `Shape.Circle`, `Body.mass`, `Shape.Circle.radius`, or a local's name.
    pub name: String,
    /// fn, struct, enum, field, variant, unit, param or local.
    pub kind: &'static str,
    /// The declaration's source text for items; the type for fields, parameters and locals.
    pub detail: String,
}

/// What the checker learned about a file, kept even when it has errors: every declaration,
/// every use of a declared name, and the type of every expression. `anvil type-at` and
/// `anvil refs` answer from these.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub defs: Vec<Def>,
    /// (span of the use, span of the declaration it names)
    pub uses: Vec<(Span, Span)>,
    /// (span of the expression, its type as shown in errors)
    pub exprs: Vec<(Span, String)>,
}

/// Everything `check` finds; `program` is None when there are errors.
pub struct Analysis {
    pub program: Option<Program>,
    pub diags: Vec<Diagnostic>,
    pub facts: Facts,
}

pub struct Checker<'a> {
    src: &'a SourceFile,
    diags: Vec<Diagnostic>,
    facts: Facts,
    /// Declaration spans of types, struct fields, enum variants and their fields, and units.
    type_spans: HashMap<String, Span>,
    field_spans: HashMap<(StructId, usize), Span>,
    variant_spans: HashMap<(EnumId, usize), Span>,
    variant_field_spans: HashMap<(EnumId, usize, usize), Span>,
    unit_spans: HashMap<String, Span>,
    units: HashMap<String, Unit>,
    structs: Vec<StructDef>,
    struct_index: HashMap<String, StructId>,
    enums: Vec<EnumDef>,
    enum_index: HashMap<String, EnumId>,
    /// Instantiations of Option/Result, by display name.
    builtin_enums: HashMap<String, EnumId>,
    sigs: Vec<FuncSig>,
    func_index: HashMap<String, FuncId>,
}

struct LocalMeta {
    span: Span,
    kw_span: Option<Span>,
    mutable: bool,
    is_param: bool,
    is_loop_var: bool,
    used: bool,
    assigned: bool,
    /// Replaces the default "rename to _x" fix for an unused binding (match patterns).
    rename_fix: Option<(Span, String)>,
}

/// Per-function (or per-test) state while checking a body.
struct Body {
    locals: Vec<Local>,
    meta: Vec<LocalMeta>,
    scopes: Vec<HashMap<String, LocalId>>,
    declared: HashMap<String, Span>,
    ret: Ty,
    uses_io: bool,
    used_io: bool,
    uses_insert: Span,
    loop_depth: usize,
    /// Some(local) while checking ensures clauses of a function with a return value.
    result_local: Option<LocalId>,
    in_ensures: bool,
    in_contract: bool,
}

pub fn check(src: &SourceFile, file: &ast::File) -> Result<Program, Vec<Diagnostic>> {
    let a = analyze(src, file);
    match a.program {
        Some(p) => Ok(p),
        None => Err(a.diags),
    }
}

pub fn analyze(src: &SourceFile, file: &ast::File) -> Analysis {
    let mut c = Checker {
        src,
        diags: Vec::new(),
        facts: Facts::default(),
        type_spans: HashMap::new(),
        field_spans: HashMap::new(),
        variant_spans: HashMap::new(),
        variant_field_spans: HashMap::new(),
        unit_spans: HashMap::new(),
        units: HashMap::new(),
        structs: Vec::new(),
        struct_index: HashMap::new(),
        enums: Vec::new(),
        enum_index: HashMap::new(),
        builtin_enums: HashMap::new(),
        sigs: Vec::new(),
        func_index: HashMap::new(),
    };
    let program = c.program(file);
    // Layout last, and only for an otherwise clean file: a broken file is fixed first.
    if c.diags.is_empty() {
        if let Some(d) = crate::fmt::check_layout(src, file) {
            c.diags.push(d);
        }
    }
    c.diags.sort_by_key(|d| (d.span.start, d.code));
    let program = c.diags.is_empty().then_some(program);
    Analysis { program, diags: c.diags, facts: c.facts }
}

/// Edit distance counting a swap of adjacent letters as one edit (optimal string alignment),
/// so typos like `cuont` for `count` get suggestions.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}

/// The candidate within a small edit distance of `name`, for "did you mean" suggestions.
pub fn closest<'b>(name: &str, candidates: impl Iterator<Item = &'b String>) -> Option<String> {
    let limit = (name.len() / 3).max(1);
    candidates
        .filter(|c| c.as_str() != name)
        .map(|c| (distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, c)| c.clone())
}

impl<'a> Checker<'a> {
    fn err(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    fn def(&mut self, span: Span, name: impl Into<String>, kind: &'static str, detail: impl Into<String>) {
        self.facts.defs.push(Def { span, name: name.into(), kind, detail: detail.into() });
    }

    fn note_use(&mut self, span: Span, def: Option<Span>) {
        if let Some(def) = def {
            self.facts.uses.push((span, def));
        }
    }

    fn show(&self, ty: &Ty) -> String {
        TyDisplay { ty, structs: &self.structs, enums: &self.enums }.to_string()
    }

    fn site(&self, span: Span) -> String {
        self.src.site(span)
    }

    fn text(&self, span: Span) -> String {
        self.src.slice(span).to_string()
    }

    // ---------------- declarations ----------------

    fn program(&mut self, file: &ast::File) -> Program {
        let mut unit_decls = Vec::new();
        let mut struct_decls = Vec::new();
        let mut enum_decls = Vec::new();
        let mut fn_decls = Vec::new();
        let mut test_decls = Vec::new();
        for item in &file.items {
            match item {
                ast::Item::Unit(u) => unit_decls.push(u),
                ast::Item::Struct(s) => struct_decls.push(s),
                ast::Item::Enum(e) => enum_decls.push(e),
                ast::Item::Fn(f) => fn_decls.push(&**f),
                ast::Item::Test(t) => test_decls.push(t),
            }
        }
        self.declare_units(&unit_decls);
        self.declare_types(&struct_decls, &enum_decls);
        self.declare_fns(&fn_decls);

        let mut funcs = Vec::new();
        for f in &fn_decls {
            funcs.push(self.func(f));
        }
        let mut seen_tests: HashMap<String, Span> = HashMap::new();
        let mut tests = Vec::new();
        for t in &test_decls {
            if let Some(prev) = seen_tests.insert(t.name.clone(), t.name_span) {
                self.err(
                    Diagnostic::new("E0201", t.name_span, format!("test \"{}\" is declared twice", t.name))
                        .note("first declared here", Some(prev)),
                );
            }
            tests.push(self.test(t));
        }
        let main = self.func_index.get("main").copied();
        if let Some(id) = main {
            let sig = &self.sigs[id];
            let ok = sig.params.is_empty() && matches!(sig.ret, Ty::Void | Ty::I64 | Ty::Error);
            if !ok {
                let span = sig.span;
                self.err(Diagnostic::new("E0350", span, "main takes no parameters and returns nothing or i64"));
            }
        }
        Program { structs: self.structs.clone(), enums: self.enums.clone(), funcs, tests, main }
    }

    fn check_new_name(&mut self, name: &ast::Ident, what: &str, seen: &mut HashMap<String, Span>) -> bool {
        if let Some(prev) = seen.get(&name.name) {
            let prev = *prev;
            self.err(
                Diagnostic::new("E0201", name.span, format!("{what} `{}` is declared twice", name.name))
                    .note("first declared here", Some(prev)),
            );
            return false;
        }
        seen.insert(name.name.clone(), name.span);
        true
    }

    fn declare_units(&mut self, decls: &[&ast::UnitDecl]) {
        let mut seen = HashMap::new();
        let mut aliases: HashMap<String, &ast::UnitExpr> = HashMap::new();
        for d in decls {
            if !self.check_new_name(&d.name, "unit", &mut seen) {
                continue;
            }
            self.unit_spans.insert(d.name.name.clone(), d.name.span);
            let text = self.text(d.span);
            self.def(d.name.span, d.name.name.clone(), "unit", text);
            match &d.def {
                None => {
                    self.units.insert(d.name.name.clone(), Unit::base(&d.name.name));
                }
                Some(e) => {
                    aliases.insert(d.name.name.clone(), e);
                }
            }
        }
        // Resolve aliases in source order; each may refer to others, cycles are errors.
        for d in decls {
            if d.def.is_some() && !self.units.contains_key(&d.name.name) {
                let mut visiting = HashSet::new();
                self.resolve_alias(&d.name.name, &aliases, &mut visiting);
            }
        }
    }

    fn resolve_alias(
        &mut self,
        name: &str,
        aliases: &HashMap<String, &ast::UnitExpr>,
        visiting: &mut HashSet<String>,
    ) -> Option<Unit> {
        if let Some(u) = self.units.get(name) {
            return Some(u.clone());
        }
        let expr = *aliases.get(name)?;
        if !visiting.insert(name.to_string()) {
            self.err(Diagnostic::new("E0201", expr.span, format!("unit `{name}` is defined in terms of itself")));
            return None;
        }
        let mut u = Unit::dimensionless();
        for (term, exp) in &expr.terms {
            let def = self.unit_spans.get(&term.name).copied();
            self.note_use(term.span, def);
            let t = if aliases.contains_key(&term.name) && !self.units.contains_key(&term.name) {
                self.resolve_alias(&term.name, aliases, visiting)?
            } else {
                self.lookup_unit(term)?
            };
            u = u.mul(&t.pow(*exp));
        }
        self.units.insert(name.to_string(), u.clone());
        Some(u)
    }

    fn lookup_unit(&mut self, name: &ast::Ident) -> Option<Unit> {
        if let Some(u) = self.units.get(&name.name) {
            let u = u.clone();
            let def = self.unit_spans.get(&name.name).copied();
            self.note_use(name.span, def);
            return Some(u);
        }
        let mut d = Diagnostic::new("E0200", name.span, format!("unknown unit `{}`", name.name));
        if let Some(s) = closest(&name.name, self.units.keys()) {
            d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), name.span, s);
        } else {
            d = d.note(format!("declare it at the top level: unit {};", name.name), None);
        }
        self.err(d);
        None
    }

    fn unit_expr(&mut self, e: &ast::UnitExpr) -> Option<Unit> {
        let mut u = Unit::dimensionless();
        let mut ok = true;
        for (term, exp) in &e.terms {
            match self.lookup_unit(term) {
                Some(t) => u = u.mul(&t.pow(*exp)),
                None => ok = false,
            }
        }
        ok.then_some(u)
    }

    fn resolve_type(&mut self, t: &ast::TypeExpr) -> Ty {
        let (name, unit, args) = match &t.kind {
            TypeKind::Array(elem) => {
                let e = self.resolve_type(elem);
                return if e == Ty::Error { Ty::Error } else { Ty::Array(Box::new(e)) };
            }
            TypeKind::Named { name, unit, args } => (name, unit, args),
        };
        let n = name.name.as_str();
        if n == "Option" || n == "Result" {
            let (want, shape) = if n == "Option" { (1, "Option[T]") } else { (2, "Result[T, E]") };
            if args.len() != want {
                self.err(Diagnostic::new("E0307", t.span, format!("{n} takes {want} type argument(s): {shape}")));
                return Ty::Error;
            }
            let tys: Vec<Ty> = args.iter().map(|a| self.resolve_type(a)).collect();
            if tys.contains(&Ty::Error) {
                return Ty::Error;
            }
            return if n == "Option" {
                self.option_of(tys[0].clone())
            } else {
                self.result_of(tys[0].clone(), tys[1].clone())
            };
        }
        if !args.is_empty() {
            let msg = if matches!(n, "i64" | "bool" | "str") {
                format!("only f64 carries units, not {n}")
            } else {
                format!("`{n}` takes no type arguments")
            };
            self.err(Diagnostic::new("E0307", t.span, msg));
            return Ty::Error;
        }
        match n {
            "i64" => Ty::I64,
            "bool" => Ty::Bool,
            "str" => Ty::Str,
            "f64" => match unit {
                None => Ty::F64(Unit::dimensionless()),
                Some(u) => self.unit_expr(u).map(Ty::F64).unwrap_or(Ty::Error),
            },
            _ => {
                if let Some(id) = self.struct_index.get(n).copied() {
                    let def = self.type_spans.get(n).copied();
                    self.note_use(name.span, def);
                    return Ty::Struct(id);
                }
                if let Some(id) = self.enum_index.get(n).copied() {
                    let def = self.type_spans.get(n).copied();
                    self.note_use(name.span, def);
                    return Ty::Enum(id);
                }
                let mut cands: Vec<String> = TYPE_NAMES.iter().map(|s| s.to_string()).collect();
                cands.extend(self.struct_index.keys().cloned());
                cands.extend(self.enum_index.keys().cloned());
                let mut d = Diagnostic::new("E0307", name.span, format!("unknown type `{n}`"));
                if let Some(s) = closest(n, cands.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), name.span, s);
                }
                self.err(d);
                Ty::Error
            }
        }
    }

    fn builtin_enum(&mut self, name: String, variants: Vec<VariantDef>) -> Ty {
        if let Some(id) = self.builtin_enums.get(&name) {
            return Ty::Enum(*id);
        }
        let id = self.enums.len();
        self.enums.push(EnumDef { name: name.clone(), variants });
        self.builtin_enums.insert(name, id);
        Ty::Enum(id)
    }

    fn option_of(&mut self, t: Ty) -> Ty {
        let name = format!("Option[{}]", self.show(&t));
        let variants = vec![
            VariantDef { name: "Some".into(), fields: vec![("value".into(), t)] },
            VariantDef { name: "None".into(), fields: Vec::new() },
        ];
        self.builtin_enum(name, variants)
    }

    fn result_of(&mut self, t: Ty, e: Ty) -> Ty {
        let name = format!("Result[{}, {}]", self.show(&t), self.show(&e));
        let variants = vec![
            VariantDef { name: "Ok".into(), fields: vec![("value".into(), t)] },
            VariantDef { name: "Err".into(), fields: vec![("error".into(), e)] },
        ];
        self.builtin_enum(name, variants)
    }

    /// "Option" or "Result" for instantiations of the built-in enums.
    fn builtin_kind(&self, id: EnumId) -> Option<&'static str> {
        let name = &self.enums[id].name;
        if !self.builtin_enums.contains_key(name) {
            None
        } else if name.starts_with("Option[") {
            Some("Option")
        } else {
            Some("Result")
        }
    }

    fn declare_types(&mut self, struct_decls: &[&ast::StructDecl], enum_decls: &[&ast::EnumDecl]) {
        let mut seen = HashMap::new();
        let mut kept = Vec::new();
        for d in struct_decls {
            if TYPE_NAMES.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in type", d.name.name)));
                continue;
            }
            if self.check_new_name(&d.name, "type", &mut seen) {
                self.type_spans.insert(d.name.name.clone(), d.name.span);
                self.def(d.name.span, d.name.name.clone(), "struct", format!("struct {}", d.name.name));
                self.struct_index.insert(d.name.name.clone(), self.structs.len());
                self.structs.push(StructDef { name: d.name.name.clone(), fields: Vec::new() });
                kept.push(*d);
            }
        }
        let mut kept_enums = Vec::new();
        for d in enum_decls {
            if TYPE_NAMES.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in type", d.name.name)));
                continue;
            }
            if self.check_new_name(&d.name, "type", &mut seen) {
                self.type_spans.insert(d.name.name.clone(), d.name.span);
                self.def(d.name.span, d.name.name.clone(), "enum", format!("enum {}", d.name.name));
                self.enum_index.insert(d.name.name.clone(), self.enums.len());
                kept_enums.push((self.enums.len(), *d));
                self.enums.push(EnumDef { name: d.name.name.clone(), variants: Vec::new() });
            }
        }
        for (id, d) in kept.iter().enumerate() {
            let mut fseen = HashMap::new();
            let mut fields = Vec::new();
            for (i, (f, t)) in d.fields.iter().enumerate() {
                self.check_new_name(f, "field", &mut fseen);
                let ty = self.resolve_type(t);
                self.field_spans.insert((id, i), f.span);
                let shown = self.show(&ty);
                self.def(f.span, format!("{}.{}", d.name.name, f.name), "field", shown);
                fields.push((f.name.clone(), ty));
            }
            self.structs[id].fields = fields;
        }
        for (id, d) in &kept_enums {
            let mut vseen = HashMap::new();
            let mut variants = Vec::new();
            for (vi, v) in d.variants.iter().enumerate() {
                self.check_new_name(&v.name, "variant", &mut vseen);
                self.variant_spans.insert((*id, vi), v.name.span);
                let vname = format!("{}.{}", d.name.name, v.name.name);
                let text = self.text(v.span);
                self.def(v.name.span, vname.clone(), "variant", text);
                let mut fseen = HashMap::new();
                let mut fields = Vec::new();
                for (fi, (f, t)) in v.fields.iter().enumerate() {
                    self.check_new_name(f, "field", &mut fseen);
                    let ty = self.resolve_type(t);
                    self.variant_field_spans.insert((*id, vi, fi), f.span);
                    let shown = self.show(&ty);
                    self.def(f.span, format!("{vname}.{}", f.name), "field", shown);
                    fields.push((f.name.clone(), ty));
                }
                variants.push(VariantDef { name: v.name.name.clone(), fields });
            }
            self.enums[*id].variants = variants;
        }
        // A type containing itself by value has no size; arrays are the way to nest.
        for (id, d) in kept.iter().enumerate() {
            let me = Ty::Struct(id);
            if self.contains_by_value(&me, &me, &mut Vec::new()) {
                self.err(
                    Diagnostic::new("E0308", d.name.span, format!("struct `{}` contains itself", d.name.name))
                        .note("hold the inner values in an array instead: [T]", None),
                );
            }
        }
        for (id, d) in &kept_enums {
            let me = Ty::Enum(*id);
            if self.contains_by_value(&me, &me, &mut Vec::new()) {
                self.err(
                    Diagnostic::new("E0308", d.name.span, format!("enum `{}` contains itself", d.name.name))
                        .note("hold the inner values in an array instead: [T]", None),
                );
            }
        }
    }

    /// Types stored inline in a value of `ty` (array elements live elsewhere).
    fn inline_children(&self, ty: &Ty) -> Vec<Ty> {
        match ty {
            Ty::Struct(id) => self.structs[*id].fields.iter().map(|(_, t)| t.clone()).collect(),
            Ty::Enum(id) => {
                self.enums[*id].variants.iter().flat_map(|v| v.fields.iter().map(|(_, t)| t.clone())).collect()
            }
            _ => Vec::new(),
        }
    }

    fn contains_by_value(&self, outer: &Ty, target: &Ty, seen: &mut Vec<Ty>) -> bool {
        for c in self.inline_children(outer) {
            if c == *target {
                return true;
            }
            if !seen.contains(&c) {
                seen.push(c.clone());
                if self.contains_by_value(&c, target, seen) {
                    return true;
                }
            }
        }
        false
    }

    fn effects(&mut self, uses: &[ast::Ident]) -> bool {
        let mut io = false;
        for u in uses {
            if u.name == "io" {
                if io {
                    self.err(Diagnostic::new("E0201", u.span, "effect `io` is listed twice"));
                }
                io = true;
            } else {
                self.err(Diagnostic::new(
                    "E0332",
                    u.span,
                    format!("unknown effect `{}`; the only effect is io", u.name),
                ));
            }
        }
        io
    }

    fn declare_fns(&mut self, decls: &[&ast::FnDecl]) {
        let mut seen = HashMap::new();
        for d in decls {
            if BUILTINS.contains(&d.name.name.as_str()) || CONSTRUCTORS.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in function", d.name.name)));
            }
            let fresh = self.check_new_name(&d.name, "function", &mut seen);
            let params = d.params.iter().map(|p| (p.name.name.clone(), self.resolve_type(&p.ty))).collect();
            let ret = d.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Void);
            let uses_io = self.effects(&d.uses);
            if fresh {
                self.func_index.insert(d.name.name.clone(), self.sigs.len());
            }
            let header = self.text(Span::new(d.span.start, d.body.span.start)).trim_end().to_string();
            self.def(d.name.span, d.name.name.clone(), "fn", header);
            // Duplicates still get a slot so FuncIds line up with fn_decls order.
            self.sigs.push(FuncSig { params, ret, uses_io, span: d.name.span });
        }
    }

    // ---------------- bodies ----------------

    fn new_body(&self, ret: Ty, uses_io: bool, uses_insert: Span) -> Body {
        Body {
            locals: Vec::new(),
            meta: Vec::new(),
            scopes: vec![HashMap::new()],
            declared: HashMap::new(),
            ret,
            uses_io,
            used_io: false,
            uses_insert,
            loop_depth: 0,
            result_local: None,
            in_ensures: false,
            in_contract: false,
        }
    }

    fn declare_local(&mut self, b: &mut Body, name: &ast::Ident, ty: Ty, meta: LocalMeta) -> LocalId {
        if let Some(prev) = b.declared.get(&name.name) {
            let prev = *prev;
            self.err(
                Diagnostic::new("E0202", name.span, format!("`{}` is already declared in this function", name.name))
                    .note("first declared here", Some(prev)),
            );
        } else if self.func_index.contains_key(&name.name) {
            self.err(Diagnostic::new("E0202", name.span, format!("`{}` is the name of a function", name.name)));
        } else if self.struct_index.contains_key(&name.name)
            || self.enum_index.contains_key(&name.name)
            || CONSTRUCTORS.contains(&name.name.as_str())
        {
            self.err(Diagnostic::new(
                "E0202",
                name.span,
                format!("`{}` is the name of a type or constructor", name.name),
            ));
        }
        b.declared.insert(name.name.clone(), name.span);
        let kind = if meta.is_param { "param" } else { "local" };
        let shown = self.show(&ty);
        self.def(name.span, name.name.clone(), kind, shown);
        let id = b.locals.len();
        b.locals.push(Local { name: name.name.clone(), ty });
        b.meta.push(meta);
        b.scopes.last_mut().unwrap().insert(name.name.clone(), id);
        id
    }

    /// A compiler-made local (match scrutinee, loop snapshot, `?` value). Never reported unused.
    fn hidden(&mut self, b: &mut Body, ty: Ty) -> LocalId {
        let id = b.locals.len();
        b.locals.push(Local { name: format!("${id}"), ty });
        b.meta.push(LocalMeta {
            span: Span::default(),
            kw_span: None,
            mutable: false,
            is_param: false,
            is_loop_var: false,
            used: true,
            assigned: false,
            rename_fix: None,
        });
        id
    }

    fn finish_locals(&mut self, b: &Body) {
        for (id, m) in b.meta.iter().enumerate() {
            let name = &b.locals[id].name;
            if !m.used && !name.starts_with('_') {
                let mut d = Diagnostic::new("E0340", m.span, format!("`{name}` is never used"));
                if m.is_param {
                    d = d.note(
                        "rename the parameter to start with _ if it is unused on purpose (callers name it too)",
                        None,
                    );
                } else if let Some((span, text)) = &m.rename_fix {
                    d = d.fix1(FixKind::ChangesMeaning, "ignore the field with `_`", *span, text.clone());
                } else {
                    d = d.fix1(
                        FixKind::ChangesMeaning,
                        format!("rename to `_{name}` if unused on purpose"),
                        m.span,
                        format!("_{name}"),
                    );
                }
                self.err(d);
            } else if m.mutable && !m.assigned && !m.is_loop_var {
                let mut d = Diagnostic::new("E0341", m.span, format!("`{name}` is declared var but never reassigned"));
                if let Some(kw) = m.kw_span {
                    d = d.fix1(FixKind::Mechanical, "declare it with let", kw, "let");
                }
                self.err(d);
            }
        }
    }

    fn check_effect_usage(&mut self, b: &Body, uses_span: Option<Span>, uses_insert: Span) {
        if b.uses_io && !b.used_io {
            if let Some(s) = uses_span {
                self.err(Diagnostic::new("E0331", s, "declares uses io but never performs io").fix1(
                    FixKind::Mechanical,
                    "remove `uses io`",
                    Span::new(uses_insert.start, s.end),
                    "",
                ));
            }
        }
    }

    /// Valid `allow LINT "reason"` lines as (lint, reason); `fn_start` is where the `fn` keyword begins.
    fn allows(&mut self, decls: &[ast::AllowDecl], fn_start: usize) -> Vec<(String, String)> {
        let mut allows: Vec<(String, String)> = Vec::new();
        for (i, a) in decls.iter().enumerate() {
            // Removing a line takes the space up to the next line's text with it.
            let line = Span::new(a.span.start, decls.get(i + 1).map_or(fn_start, |n| n.span.start));
            let name = &a.lint.name;
            if diag::lint(name).is_none() {
                let lints: Vec<String> = diag::LINTS.iter().map(|(n, _)| n.to_string()).collect();
                let mut d = Diagnostic::new("E0500", a.lint.span, format!("unknown lint `{name}`"));
                if let Some(s) = closest(name, lints.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), a.lint.span, s);
                } else {
                    d = d.note(format!("the lints are: {}", lints.join(", ")), None);
                }
                self.err(d);
            } else if a.reason.trim().is_empty() {
                self.err(Diagnostic::new("E0501", a.reason_span, format!("`allow {name}` needs a reason")));
            } else if allows.iter().any(|(l, _)| l == name) {
                self.err(
                    Diagnostic::new("E0502", a.span, format!("`{name}` is already allowed for this function")).fix1(
                        FixKind::Mechanical,
                        "remove the duplicate allow",
                        line,
                        "",
                    ),
                );
            } else {
                allows.push((name.clone(), a.reason.clone()));
            }
        }
        allows
    }

    fn func(&mut self, d: &ast::FnDecl) -> Func {
        let id = self.sigs.iter().position(|s| s.span == d.name.span).unwrap();
        let ret = self.sigs[id].ret.clone();
        let uses_io = self.sigs[id].uses_io;
        let mut b = self.new_body(ret.clone(), uses_io, d.uses_insert);
        let mut params = Vec::new();
        for (i, p) in d.params.iter().enumerate() {
            let ty = self.sigs[id].params[i].1.clone();
            let meta = LocalMeta {
                span: p.name.span,
                kw_span: None,
                mutable: false,
                is_param: true,
                is_loop_var: false,
                used: false,
                assigned: false,
                rename_fix: None,
            };
            params.push(self.declare_local(&mut b, &p.name, ty, meta));
        }
        b.in_contract = true;
        let mut requires = Vec::new();
        for e in &d.requires {
            let x = self.expect(&mut b, e, &Ty::Bool);
            let message = format!("requires failed at {}: {}", self.site(e.span), self.text(e.span));
            requires.push(Check { expr: x, message });
        }
        let mut ensures = Vec::new();
        let mut result_local = None;
        if !d.ensures.is_empty() && ret != Ty::Void {
            let id = b.locals.len();
            b.locals.push(Local { name: "result".into(), ty: ret.clone() });
            b.meta.push(LocalMeta {
                span: d.name.span,
                kw_span: None,
                mutable: false,
                is_param: false,
                is_loop_var: false,
                used: true,
                assigned: false,
                rename_fix: None,
            });
            result_local = Some(id);
        }
        b.result_local = result_local;
        b.in_ensures = true;
        for e in &d.ensures {
            let x = self.expect(&mut b, e, &Ty::Bool);
            let message = format!("ensures failed at {}: {}", self.site(e.span), self.text(e.span));
            ensures.push(Check { expr: x, message });
        }
        b.in_ensures = false;
        b.in_contract = false;
        let body = self.block(&mut b, &d.body);
        if ret != Ty::Void && ret != Ty::Error && !always_returns(&d.body.stmts) {
            self.err(
                Diagnostic::new(
                    "E0320",
                    d.body.span.at_end().to(Span::new(d.body.span.end - 1, d.body.span.end)),
                    format!("`{}` must return {} on every path", d.name.name, self.show(&ret)),
                )
                .note("the end of the function body can be reached without a return", None),
            );
        }
        self.finish_locals(&b);
        self.check_effect_usage(&b, d.uses_span, d.uses_insert);
        let allows = self.allows(&d.allows, d.span.start);
        Func {
            name: d.name.name.clone(),
            span: d.name.span,
            allows,
            params,
            ret,
            uses_io,
            requires,
            ensures,
            result_local,
            locals: b.locals,
            body,
        }
    }

    fn test(&mut self, t: &ast::TestDecl) -> Test {
        let uses_io = self.effects(&t.uses);
        let mut b = self.new_body(Ty::Void, uses_io, t.uses_insert);
        let body = self.block(&mut b, &t.body);
        self.finish_locals(&b);
        self.check_effect_usage(&b, t.uses_span, t.uses_insert);
        Test { name: t.name.clone(), uses_io, locals: b.locals, body }
    }

    fn block(&mut self, b: &mut Body, block: &ast::Block) -> Vec<Stmt> {
        b.scopes.push(HashMap::new());
        let mut out = Vec::new();
        let mut terminated: Option<Span> = None;
        for s in &block.stmts {
            if let Some(t) = terminated {
                let last = block.stmts.last().unwrap().span;
                self.err(
                    Diagnostic::new("E0323", s.span, "unreachable code")
                        .note("control never gets past this statement", Some(t))
                        .fix1(FixKind::ChangesMeaning, "delete the unreachable statements", s.span.to(last), ""),
                );
                break;
            }
            if matches!(s.kind, StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue) {
                terminated = Some(s.span);
            }
            if let Some(st) = self.stmt(b, s) {
                out.push(st);
            }
        }
        b.scopes.pop();
        out
    }

    fn stmt(&mut self, b: &mut Body, s: &ast::Stmt) -> Option<Stmt> {
        Some(match &s.kind {
            StmtKind::Let { mutable, kw_span, name, ty, init } => {
                let t = self.resolve_type(ty);
                let e = self.value(b, init, &t);
                let meta = LocalMeta {
                    span: name.span,
                    kw_span: Some(*kw_span),
                    mutable: *mutable,
                    is_param: false,
                    is_loop_var: false,
                    used: false,
                    assigned: false,
                    rename_fix: None,
                };
                let id = self.declare_local(b, name, t, meta);
                Stmt::Let(id, e)
            }
            StmtKind::Assign { target, value } => {
                let (local, path, ty) = self.place(b, target)?;
                let v = self.value(b, value, &ty);
                Stmt::Assign(local, path, v)
            }
            StmtKind::Expr(e) => {
                let (AK::Call { .. } | AK::Method { .. }) = &e.kind else {
                    self.err(Diagnostic::new(
                        "E0392",
                        e.span,
                        "this expression does nothing; only calls can be statements",
                    ));
                    return None;
                };
                let x = self.expr(b, e);
                if x.ty != Ty::Void && x.ty != Ty::Error {
                    let ty = self.show(&x.ty);
                    self.err(
                        Diagnostic::new("E0392", e.span, format!("the {ty} returned by this call is dropped")).fix1(
                            FixKind::ChangesMeaning,
                            "bind it to a name",
                            e.span.at_start(),
                            format!("let _unused: {ty} = "),
                        ),
                    );
                }
                Stmt::Expr(x)
            }
            StmtKind::If { cond, then, else_ } => {
                let c = self.condition(b, cond);
                let t = self.block(b, then);
                let e = match else_.as_deref() {
                    None => Vec::new(),
                    Some(Else::Block(blk)) => self.block(b, blk),
                    Some(Else::If(st)) => self.stmt(b, st).into_iter().collect(),
                };
                Stmt::If(c, t, e)
            }
            StmtKind::While { cond, body } => {
                let c = self.condition(b, cond);
                b.loop_depth += 1;
                let body = self.block(b, body);
                b.loop_depth -= 1;
                Stmt::While(c, body)
            }
            StmtKind::For { var, start, end, body } => {
                let s0 = self.expect(b, start, &Ty::I64);
                let e0 = self.expect(b, end, &Ty::I64);
                b.scopes.push(HashMap::new());
                let meta = LocalMeta {
                    span: var.span,
                    kw_span: None,
                    mutable: false,
                    is_param: false,
                    is_loop_var: true,
                    used: false,
                    assigned: false,
                    rename_fix: None,
                };
                let id = self.declare_local(b, var, Ty::I64, meta);
                b.loop_depth += 1;
                let body = self.block(b, body);
                b.loop_depth -= 1;
                b.scopes.pop();
                Stmt::For(id, s0, e0, body)
            }
            StmtKind::ForEach { var, iter, body } => {
                let it = self.expr(b, iter);
                let elem = match &it.ty {
                    Ty::Array(t) => (**t).clone(),
                    Ty::Error => Ty::Error,
                    other => {
                        let t = self.show(other);
                        self.err(Diagnostic::new(
                            "E0300",
                            iter.span,
                            format!("for loops run over an array or a range start..end, not {t}"),
                        ));
                        Ty::Error
                    }
                };
                let held = self.hidden(b, it.ty.clone());
                b.scopes.push(HashMap::new());
                let meta = LocalMeta {
                    span: var.span,
                    kw_span: None,
                    mutable: false,
                    is_param: false,
                    is_loop_var: true,
                    used: false,
                    assigned: false,
                    rename_fix: None,
                };
                let id = self.declare_local(b, var, elem, meta);
                b.loop_depth += 1;
                let body = self.block(b, body);
                b.loop_depth -= 1;
                b.scopes.pop();
                Stmt::ForEach { var: id, held, iter: it, body }
            }
            StmtKind::Match { scrutinee, arms, span_end } => return self.match_stmt(b, scrutinee, arms, *span_end),
            StmtKind::Return(v) => {
                let ret = b.ret.clone();
                match (v, &ret) {
                    (None, Ty::Void) => Stmt::Return(None),
                    (Some(e), Ty::Void) => {
                        self.err(
                            Diagnostic::new(
                                "E0321",
                                e.span,
                                "this function has no return type, so return takes no value",
                            )
                            .fix1(
                                FixKind::ChangesMeaning,
                                "drop the value",
                                Span::new(s.span.start + 6, e.span.end),
                                "",
                            ),
                        );
                        None?
                    }
                    (None, _) => {
                        let t = self.show(&ret);
                        self.err(Diagnostic::new("E0321", s.span, format!("this function must return a {t}")));
                        None?
                    }
                    (Some(e), _) => Stmt::Return(Some(self.value(b, e, &ret))),
                }
            }
            StmtKind::Break | StmtKind::Continue => {
                if b.loop_depth == 0 {
                    self.err(Diagnostic::new("E0370", s.span, "break and continue only work inside a loop"));
                    return None;
                }
                if matches!(s.kind, StmtKind::Break) {
                    Stmt::Break
                } else {
                    Stmt::Continue
                }
            }
        })
    }

    fn match_stmt(&mut self, b: &mut Body, scrutinee: &ast::Expr, arms: &[ast::Arm], span_end: Span) -> Option<Stmt> {
        let x = self.expr(b, scrutinee);
        let id = match &x.ty {
            Ty::Enum(id) => *id,
            Ty::Error => return None,
            other => {
                let t = self.show(other);
                self.err(
                    Diagnostic::new("E0403", scrutinee.span, format!("match needs an enum value, found {t}"))
                        .note("use if and else for i64, f64, bool and str", None),
                );
                for arm in arms {
                    self.unchecked_arm(b, arm);
                }
                return None;
            }
        };
        let variants = self.enums[id].variants.clone();
        let ename = self.enums[id].name.clone();
        let mut seen: HashMap<usize, Span> = HashMap::new();
        let mut unknown_arm = false;
        let mut out = Vec::new();
        for arm in arms {
            let Some(vi) = variants.iter().position(|v| v.name == arm.variant.name) else {
                unknown_arm = true;
                let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                let mut d = Diagnostic::new(
                    "E0200",
                    arm.variant.span,
                    format!("`{ename}` has no variant `{}`", arm.variant.name),
                )
                .note(format!("variants: {}", names.join(", ")), None);
                if let Some(s) = closest(&arm.variant.name, names.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), arm.variant.span, s);
                }
                self.err(d);
                self.unchecked_arm(b, arm);
                continue;
            };
            if let Some(prev) = seen.insert(vi, arm.variant.span) {
                self.err(
                    Diagnostic::new("E0401", arm.variant.span, format!("`{}` is matched twice", arm.variant.name))
                        .note("first matched here", Some(prev)),
                );
                self.unchecked_arm(b, arm);
                continue;
            }
            let def = self.variant_spans.get(&(id, vi)).copied();
            self.note_use(arm.variant.span, def);
            b.scopes.push(HashMap::new());
            let bindings = self.pattern(b, arm, (id, vi), &variants[vi]);
            let body = self.block(b, &arm.body);
            b.scopes.pop();
            out.push(Arm { variant: vi, bindings, body });
        }
        let missing: Vec<&VariantDef> =
            variants.iter().enumerate().filter(|(i, _)| !seen.contains_key(i)).map(|(_, v)| v).collect();
        if !missing.is_empty() && !unknown_arm {
            let names: Vec<&str> = missing.iter().map(|v| v.name.as_str()).collect();
            let arm_text: Vec<String> = missing
                .iter()
                .map(|v| {
                    if v.fields.is_empty() {
                        format!("{} => {{}}", v.name)
                    } else {
                        let fs: Vec<String> = v.fields.iter().map(|(f, _)| format!("{f}: _")).collect();
                        format!("{}({}) => {{}}", v.name, fs.join(", "))
                    }
                })
                .collect();
            // Insert before the closing brace, one arm per line when the brace starts its line.
            let line_start = self.src.text[..span_end.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let before = &self.src.text[line_start..span_end.start];
            let text = if before.trim().is_empty() {
                arm_text.iter().map(|a| format!("{INDENT}{a}\n{before}")).collect::<String>()
            } else {
                arm_text.iter().map(|a| format!("{a} ")).collect::<String>()
            };
            self.err(
                Diagnostic::new(
                    "E0400",
                    scrutinee.span,
                    format!("match on `{ename}` does not handle {}", names.join(", ")),
                )
                .note("every variant needs its own arm; there is no catch-all", None)
                .fix1(
                    FixKind::ChangesMeaning,
                    "add empty arms for the missing variants",
                    span_end.at_start(),
                    text,
                ),
            );
        }
        let held = self.hidden(b, x.ty.clone());
        Some(Stmt::Match { held, scrutinee: x, arms: out })
    }

    /// Checks the body of an arm whose pattern cannot be typed, so its own errors are still
    /// found; its bindings get the error type and are never reported unused.
    fn unchecked_arm(&mut self, b: &mut Body, arm: &ast::Arm) {
        b.scopes.push(HashMap::new());
        for pf in arm.fields.iter().flatten() {
            let name = pf.binding.clone().unwrap_or_else(|| pf.field.clone());
            if name.name != "_" {
                self.error_binding(b, &name);
            }
        }
        self.block(b, &arm.body);
        b.scopes.pop();
    }

    fn error_binding(&mut self, b: &mut Body, name: &ast::Ident) {
        let meta = LocalMeta {
            span: name.span,
            kw_span: None,
            mutable: false,
            is_param: false,
            is_loop_var: false,
            used: true,
            assigned: false,
            rename_fix: None,
        };
        self.declare_local(b, name, Ty::Error, meta);
    }

    /// Checks an arm's field list against the variant and declares its bindings.
    fn pattern(&mut self, b: &mut Body, arm: &ast::Arm, at: (EnumId, usize), v: &VariantDef) -> Vec<(usize, LocalId)> {
        let vname = &arm.variant.name;
        let fields = match (&arm.fields, v.fields.is_empty()) {
            (None, true) => return Vec::new(),
            (Some(fs), true) => {
                let mut d = Diagnostic::new("E0402", arm.fields_span, format!("`{vname}` has no fields"));
                if fs.is_empty() {
                    d = d.fix1(FixKind::Mechanical, "remove the parentheses", arm.fields_span, "");
                }
                self.err(d);
                return Vec::new();
            }
            (None, false) => {
                let list: Vec<String> = v.fields.iter().map(|(f, _)| format!("{f}: _")).collect();
                self.err(
                    Diagnostic::new(
                        "E0402",
                        arm.variant.span,
                        format!("`{vname}` has fields; list them in the pattern"),
                    )
                    .fix1(
                        FixKind::ChangesMeaning,
                        "list the fields, ignoring each",
                        arm.variant.span.at_end(),
                        format!("({})", list.join(", ")),
                    ),
                );
                return Vec::new();
            }
            (Some(fs), false) => fs,
        };
        let decl: Vec<&str> = v.fields.iter().map(|(f, _)| f.as_str()).collect();
        let mut ok = true;
        let mut given: HashMap<&str, usize> = HashMap::new();
        for (i, pf) in fields.iter().enumerate() {
            if !decl.contains(&pf.field.name.as_str()) {
                ok = false;
                let owned: Vec<String> = decl.iter().map(|s| s.to_string()).collect();
                let mut d =
                    Diagnostic::new("E0402", pf.field.span, format!("`{vname}` has no field `{}`", pf.field.name))
                        .note(format!("fields: {}", decl.join(", ")), None);
                if let Some(s) = closest(&pf.field.name, owned.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), pf.field.span, s);
                }
                self.err(d);
            } else if given.insert(pf.field.name.as_str(), i).is_some() {
                ok = false;
                self.err(Diagnostic::new("E0402", pf.field.span, format!("field `{}` is listed twice", pf.field.name)));
            }
        }
        if !ok {
            for pf in fields {
                let name = pf.binding.clone().unwrap_or_else(|| pf.field.clone());
                if name.name != "_" {
                    self.error_binding(b, &name);
                }
            }
            return Vec::new();
        }
        let missing: Vec<&str> = decl.iter().filter(|f| !given.contains_key(**f)).copied().collect();
        if !missing.is_empty() {
            // Rewrite the whole list in declaration order, ignoring the missing fields.
            let text: Vec<String> = decl
                .iter()
                .map(|d| match given.get(d) {
                    Some(i) => self.text(fields[*i].span),
                    None => format!("{d}: _"),
                })
                .collect();
            let inner = Span::new(arm.fields_span.start + 1, arm.fields_span.end - 1);
            self.err(
                Diagnostic::new(
                    "E0402",
                    arm.fields_span,
                    format!("pattern for `{vname}` leaves out {}", missing.join(", ")),
                )
                .note("list every field; write `field: _` to ignore one", None)
                .fix1(FixKind::ChangesMeaning, "ignore the missing fields", inner, text.join(", ")),
            );
            ok = false;
        }
        let in_order = fields.iter().zip(&decl).all(|(pf, d)| pf.field.name == *d);
        if ok && !in_order {
            let text: Vec<String> = decl.iter().map(|d| self.text(fields[given[d]].span)).collect();
            let inner = Span::new(fields[0].span.start, fields.last().unwrap().span.end);
            self.err(
                Diagnostic::new("E0402", arm.fields_span, format!("fields of `{vname}` must be in declaration order"))
                    .note(format!("declared order: {}", decl.join(", ")), None)
                    .fix1(FixKind::Mechanical, "reorder the fields", inner, text.join(", ")),
            );
            ok = false;
        }
        let mut bindings = Vec::new();
        for pf in fields {
            let name = pf.binding.clone().unwrap_or_else(|| pf.field.clone());
            if name.name == "_" {
                continue;
            }
            if !ok {
                self.error_binding(b, &name);
                continue;
            }
            let i = given[pf.field.name.as_str()];
            let def = self.variant_field_spans.get(&(at.0, at.1, i)).copied();
            self.note_use(pf.field.span, def);
            let rename_fix = match &pf.binding {
                None => Some((pf.field.span.at_end(), ": _".to_string())),
                Some(n) => Some((n.span, "_".to_string())),
            };
            let meta = LocalMeta {
                span: name.span,
                kw_span: None,
                mutable: false,
                is_param: false,
                is_loop_var: false,
                used: false,
                assigned: false,
                rename_fix,
            };
            let id = self.declare_local(b, &name, v.fields[i].1.clone(), meta);
            bindings.push((i, id));
        }
        bindings
    }

    /// The right side of let, assignment or return: the only places `?` may appear.
    fn value(&mut self, b: &mut Body, e: &ast::Expr, want: &Ty) -> Expr {
        if let AK::Try(inner) = &e.kind {
            let x = self.try_expr(b, e, inner);
            self.require_ty(e, &x, want);
            return x;
        }
        self.expect(b, e, want)
    }

    fn try_expr(&mut self, b: &mut Body, e: &ast::Expr, inner: &ast::Expr) -> Expr {
        let x = self.expr(b, inner);
        let id = match &x.ty {
            Ty::Error => return Self::err_expr(),
            Ty::Enum(id) if self.builtin_kind(*id).is_some() => *id,
            other => {
                let t = self.show(other);
                self.err(Diagnostic::new("E0405", e.span, format!("`?` needs an Option or a Result, found {t}")));
                return Self::err_expr();
            }
        };
        let kind = self.builtin_kind(id);
        let ret = b.ret.clone();
        if ret == Ty::Error {
            return Self::err_expr();
        }
        let fits = match &ret {
            Ty::Enum(rid) if self.builtin_kind(*rid) == kind => {
                kind == Some("Option")
                    || self.enums[*rid].variants[RESULT_ERR].fields[0].1
                        == self.enums[id].variants[RESULT_ERR].fields[0].1
            }
            _ => false,
        };
        if !fits {
            let rs = self.show(&ret);
            let msg = if kind == Some("Option") {
                format!("`?` on an Option returns None early, so the function must return an Option, not {rs}")
            } else {
                let es = self.show(&self.enums[id].variants[RESULT_ERR].fields[0].1.clone());
                format!("`?` on a Result returns its Err early, so the function must return Result[_, {es}], not {rs}")
            };
            self.err(Diagnostic::new("E0405", e.span, msg));
            return Self::err_expr();
        }
        let Ty::Enum(ret_enum) = ret else { unreachable!() };
        let payload = self.enums[id].variants[0].fields[0].1.clone();
        let held = self.hidden(b, x.ty.clone());
        Expr { kind: ExprKind::Try { held, inner: Box::new(x), ret_enum }, ty: payload }
    }

    fn condition(&mut self, b: &mut Body, cond: &ast::Expr) -> Expr {
        let c = self.expr(b, cond);
        if c.ty != Ty::Bool && c.ty != Ty::Error {
            let mut d = Diagnostic::new("E0380", cond.span, format!("condition is {}, not bool", self.show(&c.ty)));
            if c.ty == Ty::I64 {
                d = d.fix1(FixKind::ChangesMeaning, "compare with zero", cond.span.at_end(), " != 0");
            }
            self.err(d);
        }
        c
    }

    /// Resolves an assignment target to (local, field path, type).
    fn place(&mut self, b: &mut Body, target: &ast::Expr) -> Option<(LocalId, Vec<Step>, Ty)> {
        match &target.kind {
            AK::Name(id) => {
                let Some(local) = self.lookup_local(b, id) else {
                    self.unknown_name(b, id);
                    return None;
                };
                self.note_use(id.span, Some(b.meta[local].span));
                let m = &mut b.meta[local];
                m.assigned = true;
                if !m.mutable {
                    let mut d = Diagnostic::new(
                        "E0310",
                        target.span,
                        format!("`{}` cannot be assigned: it is not var", id.name),
                    );
                    if m.is_param {
                        d = d.note("parameters are immutable; copy into a var first", None);
                    } else if m.is_loop_var {
                        d = d.note("loop variables are immutable", None);
                    } else if let Some(kw) = m.kw_span {
                        d = d.note("declared here", Some(m.span)).fix1(
                            FixKind::ChangesMeaning,
                            "declare it with var",
                            kw,
                            "var",
                        );
                    }
                    self.err(d);
                }
                Some((local, Vec::new(), b.locals[local].ty.clone()))
            }
            AK::Field(base, field) => {
                let (local, mut path, ty) = self.place(b, base)?;
                let Ty::Struct(sid) = ty else {
                    if ty != Ty::Error {
                        let t = self.show(&ty);
                        self.err(Diagnostic::new("E0306", field.span, format!("{t} has no fields")));
                    }
                    return None;
                };
                let idx = self.field_index(sid, field)?;
                path.push(Step::Field(idx));
                Some((local, path, self.structs[sid].fields[idx].1.clone()))
            }
            AK::Index(base, index) => {
                let (local, mut path, ty) = self.place(b, base)?;
                let i = self.expect(b, index, &Ty::I64);
                match ty {
                    Ty::Array(elem) => {
                        path.push(Step::Index { index: i, site: self.site(target.span) });
                        Some((local, path, *elem))
                    }
                    Ty::Error => None,
                    other => {
                        let t = self.show(&other);
                        self.err(Diagnostic::new("E0302", target.span, format!("only arrays can be indexed, not {t}")));
                        None
                    }
                }
            }
            AK::Paren(inner) => self.place(b, inner),
            _ => {
                self.err(Diagnostic::new(
                    "E0311",
                    target.span,
                    "only variables, their fields and array elements can be assigned",
                ));
                None
            }
        }
    }

    fn field_index(&mut self, sid: StructId, field: &ast::Ident) -> Option<usize> {
        let fields = &self.structs[sid].fields;
        if let Some(i) = fields.iter().position(|(n, _)| *n == field.name) {
            let def = self.field_spans.get(&(sid, i)).copied();
            self.note_use(field.span, def);
            return Some(i);
        }
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        let sname = self.structs[sid].name.clone();
        let mut d = Diagnostic::new("E0306", field.span, format!("struct `{sname}` has no field `{}`", field.name))
            .note(format!("fields: {}", names.join(", ")), None);
        if let Some(s) = closest(&field.name, names.iter()) {
            d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), field.span, s);
        }
        self.err(d);
        None
    }

    fn lookup_local(&self, b: &Body, id: &ast::Ident) -> Option<LocalId> {
        b.scopes.iter().rev().find_map(|s| s.get(&id.name).copied())
    }

    fn unknown_name(&mut self, b: &mut Body, id: &ast::Ident) {
        let mut d = Diagnostic::new("E0200", id.span, format!("unknown name `{}`", id.name));
        if self.func_index.contains_key(&id.name) || BUILTINS.contains(&id.name.as_str()) {
            d = d.note(format!("`{}` is a function; call it with ( )", id.name), None);
        } else if self.enum_index.contains_key(&id.name) {
            d = d.note(format!("`{0}` is an enum; name a variant: {0}.Variant", id.name), None);
        } else {
            let visible: Vec<String> = b.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
            if let Some(s) = closest(&id.name, visible.iter()) {
                // The suggested variable was probably meant to be read here; don't also call it unused.
                if let Some(l) = b.scopes.iter().rev().find_map(|sc| sc.get(&s).copied()) {
                    b.meta[l].used = true;
                }
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), id.span, s);
            } else if b.declared.contains_key(&id.name) {
                d = d.note("it is declared in a block that has already ended", b.declared.get(&id.name).copied());
            }
        }
        self.err(d);
    }

    // ---------------- expressions ----------------

    /// Checks `e` against an expected type, reporting mismatches with fixes.
    fn expect(&mut self, b: &mut Body, e: &ast::Expr, want: &Ty) -> Expr {
        let x = self.expr_hint(b, e, Some(want));
        self.require_ty(e, &x, want);
        x
    }

    fn require_ty(&mut self, e: &ast::Expr, x: &Expr, want: &Ty) {
        if x.ty == *want || x.ty == Ty::Error || *want == Ty::Error {
            return;
        }
        let got_s = self.show(&x.ty);
        let want_s = self.show(want);
        let d = match (&x.ty, want) {
            (Ty::F64(_), Ty::F64(u)) => {
                let mut d = Diagnostic::new("E0301", e.span, format!("expected {want_s}, found {got_s}"));
                if let AK::Float(_, None) = &e.kind {
                    d = d.fix1(
                        FixKind::Mechanical,
                        format!("give the literal the unit {u}"),
                        e.span.at_end(),
                        format!("[{u}]"),
                    );
                }
                d
            }
            (Ty::I64, Ty::F64(u)) => {
                let mut d = Diagnostic::new("E0300", e.span, format!("expected {want_s}, found i64"));
                if let AK::Int(v) = &e.kind {
                    let unit = if u.is_dimensionless() { String::new() } else { format!("[{u}]") };
                    d = d.fix1(FixKind::Mechanical, "write a float literal", e.span, format!("{v}.0{unit}"));
                } else if u.is_dimensionless() {
                    d = d.fix(
                        FixKind::ChangesMeaning,
                        "convert with to_f64",
                        vec![
                            Edit { span: e.span.at_start(), text: "to_f64(".into() },
                            Edit { span: e.span.at_end(), text: ")".into() },
                        ],
                    );
                }
                d
            }
            (Ty::F64(u), Ty::I64) => {
                let mut d = Diagnostic::new("E0300", e.span, format!("expected i64, found {got_s}"));
                if u.is_dimensionless() {
                    d = d.fix(
                        FixKind::ChangesMeaning,
                        "truncate with to_i64",
                        vec![
                            Edit { span: e.span.at_start(), text: "to_i64(".into() },
                            Edit { span: e.span.at_end(), text: ")".into() },
                        ],
                    );
                }
                d
            }
            _ => Diagnostic::new("E0300", e.span, format!("expected {want_s}, found {got_s}")),
        };
        self.err(d);
    }

    fn err_expr() -> Expr {
        Expr { kind: ExprKind::Bool(false), ty: Ty::Error }
    }

    fn expr(&mut self, b: &mut Body, e: &ast::Expr) -> Expr {
        self.expr_hint(b, e, None)
    }

    /// `hint` is the expected type where one is known; it types `None`, `Ok`, `Err` and `[]`.
    fn expr_hint(&mut self, b: &mut Body, e: &ast::Expr, hint: Option<&Ty>) -> Expr {
        let x = self.expr_kind(b, e, hint);
        if x.ty != Ty::Error {
            let shown = self.show(&x.ty);
            self.facts.exprs.push((e.span, shown));
        }
        x
    }

    fn expr_kind(&mut self, b: &mut Body, e: &ast::Expr, hint: Option<&Ty>) -> Expr {
        let (kind, ty) = match &e.kind {
            AK::Int(v) => (ExprKind::Int(*v), Ty::I64),
            AK::Float(v, unit) => {
                let u = match unit {
                    None => Some(Unit::dimensionless()),
                    Some(u) => self.unit_expr(u),
                };
                match u {
                    Some(u) => (ExprKind::Float(*v), Ty::F64(u)),
                    None => return Self::err_expr(),
                }
            }
            AK::Bool(v) => (ExprKind::Bool(*v), Ty::Bool),
            AK::Str(s) => (ExprKind::Str(s.clone()), Ty::Str),
            AK::Paren(inner) => return self.expr_hint(b, inner, hint),
            AK::Name(id) if id.name == "None" => return self.builtin_ctor(b, e, id, None, hint),
            AK::Name(id) => match self.lookup_local(b, id) {
                Some(l) => {
                    b.meta[l].used = true;
                    self.note_use(id.span, Some(b.meta[l].span));
                    (ExprKind::Local(l), b.locals[l].ty.clone())
                }
                None => {
                    self.unknown_name(b, id);
                    return Self::err_expr();
                }
            },
            AK::Result => {
                if !b.in_ensures {
                    self.err(Diagnostic::new("E0322", e.span, "`result` is only valid in ensures clauses"));
                    return Self::err_expr();
                }
                match b.result_local {
                    Some(l) => (ExprKind::Local(l), b.locals[l].ty.clone()),
                    None => {
                        self.err(Diagnostic::new(
                            "E0322",
                            e.span,
                            "this function returns nothing, so there is no result",
                        ));
                        return Self::err_expr();
                    }
                }
            }
            AK::Field(base, field) if self.enum_name(b, base).is_some() => {
                let eid = self.enum_name(b, base).unwrap();
                return self.variant_ctor(b, e, (eid, base.span), field, None);
            }
            AK::Field(base, field) => {
                let x = self.expr(b, base);
                match x.ty.clone() {
                    Ty::Struct(sid) => match self.field_index(sid, field) {
                        Some(i) => {
                            let ty = self.structs[sid].fields[i].1.clone();
                            (ExprKind::Field(Box::new(x), i), ty)
                        }
                        None => return Self::err_expr(),
                    },
                    Ty::Error => return Self::err_expr(),
                    other => {
                        let t = self.show(&other);
                        let mut d = Diagnostic::new("E0306", field.span, format!("{t} has no fields"));
                        if let Ty::Enum(_) = other {
                            d = d.note("read a variant's fields with match", None);
                        }
                        self.err(d);
                        return Self::err_expr();
                    }
                }
            }
            AK::Method { receiver, method, args } => return self.method(b, e, receiver, method, args),
            AK::Index(base, index) => {
                let x = self.expr(b, base);
                let i = self.expect(b, index, &Ty::I64);
                match x.ty.clone() {
                    Ty::Array(elem) => {
                        let site = self.site(e.span);
                        (ExprKind::Index { base: Box::new(x), index: Box::new(i), site }, *elem)
                    }
                    Ty::Error => return Self::err_expr(),
                    other => {
                        let t = self.show(&other);
                        let mut d = Diagnostic::new("E0302", e.span, format!("only arrays can be indexed, not {t}"));
                        if other == Ty::Str {
                            d = d.note("strings are not indexed by position in Anvil", None);
                        }
                        self.err(d);
                        return Self::err_expr();
                    }
                }
            }
            AK::Array(elems) => {
                let elem_hint = match hint {
                    Some(Ty::Array(t)) => Some((**t).clone()),
                    _ => None,
                };
                let elem = match (elem_hint, elems.first()) {
                    (Some(t), _) => t,
                    (None, Some(first)) => {
                        let x = self.expr(b, first);
                        x.ty
                    }
                    (None, None) => {
                        self.err(
                            Diagnostic::new("E0404", e.span, "the element type of `[]` cannot be inferred here")
                                .note("state the type: let xs: [i64] = [];", None),
                        );
                        return Self::err_expr();
                    }
                };
                if elem == Ty::Error {
                    for x in elems {
                        self.expr(b, x);
                    }
                    return Self::err_expr();
                }
                let vals: Vec<Expr> = elems.iter().map(|x| self.expect(b, x, &elem)).collect();
                (ExprKind::ArrayLit(vals), Ty::Array(Box::new(elem)))
            }
            AK::Interp(parts) => {
                let mut out = Vec::new();
                let mut bad = false;
                for part in parts {
                    match part {
                        InterpPart::Lit(text) => out.push(Expr { kind: ExprKind::Str(text.clone()), ty: Ty::Str }),
                        InterpPart::Expr(pe) => {
                            let x = self.expr(b, pe);
                            if x.ty == Ty::Error {
                                bad = true;
                            } else if !printable(&x.ty) {
                                bad = true;
                                let t = self.show(&x.ty);
                                self.err(Diagnostic::new(
                                    "E0407",
                                    pe.span,
                                    format!("{t} has no text form; only i64, f64, bool and str can be interpolated"),
                                ));
                            }
                            out.push(x);
                        }
                    }
                }
                if bad {
                    return Self::err_expr();
                }
                (ExprKind::Interp(out), Ty::Str)
            }
            AK::Try(inner) => {
                self.expr(b, inner);
                self.err(
                    Diagnostic::new(
                        "E0405",
                        e.span,
                        "`?` can only apply to the whole value of let, var, an assignment or return",
                    )
                    .note("bind the result first: let v: T = f(x)?;", None),
                );
                return Self::err_expr();
            }
            AK::StructLit { name, fields } => return self.struct_lit(b, e, name, fields),
            AK::Call { callee, args } if CONSTRUCTORS.contains(&callee.name.as_str()) => {
                return self.builtin_ctor(b, e, callee, Some(args), hint)
            }
            AK::Call { callee, args } => return self.call(b, e, callee, args),
            AK::Unary(op, inner) => {
                let x = self.expr(b, inner);
                match (op, &x.ty) {
                    (_, Ty::Error) => return Self::err_expr(),
                    (UnOp::Neg, Ty::I64) => {
                        let overflow = overflow_message("-", &self.site(e.span));
                        (ExprKind::INeg { e: Box::new(x), overflow }, Ty::I64)
                    }
                    (UnOp::Neg, Ty::F64(u)) => {
                        let u = u.clone();
                        (ExprKind::FNeg(Box::new(x)), Ty::F64(u))
                    }
                    (UnOp::Not, Ty::Bool) => (ExprKind::Not(Box::new(x)), Ty::Bool),
                    (op, t) => {
                        let o = if *op == UnOp::Neg { "-" } else { "!" };
                        let t = self.show(t);
                        self.err(Diagnostic::new("E0302", e.span, format!("`{o}` is not defined for {t}")));
                        return Self::err_expr();
                    }
                }
            }
            AK::Binary(op, l, r) => return self.binary(b, e, *op, l, r),
        };
        Expr { kind, ty }
    }

    fn binary(&mut self, b: &mut Body, e: &ast::Expr, op: BinOp, l: &ast::Expr, r: &ast::Expr) -> Expr {
        let x = self.expr(b, l);
        let y = self.expr(b, r);
        if x.ty == Ty::Error || y.ty == Ty::Error {
            return Self::err_expr();
        }
        let site = self.site(e.span);
        let bx = |v: Expr| Box::new(v);
        let (lt, rt) = (x.ty.clone(), y.ty.clone());
        if lt == Ty::Str && rt == Ty::Str {
            if op == BinOp::Add {
                return Expr { kind: ExprKind::StrConcat(bx(x), bx(y)), ty: Ty::Str };
            }
            if op.is_comparison() {
                return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
            }
        }
        match op {
            BinOp::And | BinOp::Or => {
                if lt == Ty::Bool && rt == Ty::Bool {
                    let kind = if op == BinOp::And { ExprKind::And(bx(x), bx(y)) } else { ExprKind::Or(bx(x), bx(y)) };
                    return Expr { kind, ty: Ty::Bool };
                }
            }
            _ if lt == Ty::I64 && rt == Ty::I64 => {
                let iop = match op {
                    BinOp::Add => Some(IOp::Add),
                    BinOp::Sub => Some(IOp::Sub),
                    BinOp::Mul => Some(IOp::Mul),
                    BinOp::Div => Some(IOp::Div),
                    BinOp::Rem => Some(IOp::Rem),
                    _ => None,
                };
                if let Some(iop) = iop {
                    let kind = ExprKind::IArith {
                        op: iop,
                        l: bx(x),
                        r: bx(y),
                        overflow: overflow_message(op.text(), &site),
                        div_zero: div_zero_message(&site),
                    };
                    return Expr { kind, ty: Ty::I64 };
                }
                return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
            }
            _ => {
                if let (Ty::F64(a), Ty::F64(c)) = (&lt, &rt) {
                    let (a, c) = (a.clone(), c.clone());
                    match op {
                        BinOp::Mul => {
                            return Expr { kind: ExprKind::FArith(FOp::Mul, bx(x), bx(y)), ty: Ty::F64(a.mul(&c)) }
                        }
                        BinOp::Div => {
                            return Expr { kind: ExprKind::FArith(FOp::Div, bx(x), bx(y)), ty: Ty::F64(a.div(&c)) }
                        }
                        BinOp::Rem => {}
                        _ if a != c => {
                            let d = Diagnostic::new(
                                "E0301",
                                e.span,
                                format!(
                                    "`{}` needs matching units, found {} and {}",
                                    op.text(),
                                    self.show(&lt),
                                    self.show(&rt)
                                ),
                            );
                            self.err(self.unit_fix(d, l, r, &a, &c));
                            return Self::err_expr();
                        }
                        BinOp::Add => return Expr { kind: ExprKind::FArith(FOp::Add, bx(x), bx(y)), ty: Ty::F64(a) },
                        BinOp::Sub => return Expr { kind: ExprKind::FArith(FOp::Sub, bx(x), bx(y)), ty: Ty::F64(a) },
                        _ => return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool },
                    }
                }
                if lt == Ty::Bool && rt == Ty::Bool && matches!(op, BinOp::Eq | BinOp::Ne) {
                    return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
                }
                // i64 mixed with f64: point at the integer side.
                let mixed = match (&lt, &rt) {
                    (Ty::I64, Ty::F64(u)) => Some((l, &x, u.clone())),
                    (Ty::F64(u), Ty::I64) => Some((r, &y, u.clone())),
                    _ => None,
                };
                if let Some((side, sx, u)) = mixed {
                    if op != BinOp::Rem {
                        let msg = format!(
                            "`{}` cannot mix {} and {}; Anvil never converts implicitly",
                            op.text(),
                            self.show(&lt),
                            self.show(&rt)
                        );
                        let d = Diagnostic::new("E0300", e.span, msg);
                        let want = Ty::F64(u);
                        // Reuse the coercion fixes for the integer operand.
                        let before = self.diags.len();
                        self.require_ty(side, sx, &want);
                        let fixes = self.diags.pop().map(|d| d.fixes).unwrap_or_default();
                        debug_assert_eq!(self.diags.len(), before);
                        let mut d = d;
                        d.fixes = fixes;
                        self.err(d);
                        return Self::err_expr();
                    }
                }
            }
        }
        let (ls, rs) = (self.show(&lt), self.show(&rt));
        let mut d = Diagnostic::new("E0302", e.span, format!("`{}` is not defined for {ls} and {rs}", op.text()));
        if op == BinOp::Rem && matches!(lt, Ty::F64(_)) {
            d = d.note("% is only defined for i64", None);
        } else if lt == Ty::Str || rt == Ty::Str {
            d = d.note("build text with interpolation: \"{a} and {b}\"", None);
        } else if matches!(lt, Ty::Enum(_)) {
            d = d.note("compare enums with match", None);
        } else if matches!(lt, Ty::Array(_)) {
            d = d.note("compare arrays element by element", None);
        } else if matches!(lt, Ty::Struct(_)) {
            d = d.note("compare structs field by field", None);
        }
        self.err(d);
        Self::err_expr()
    }

    fn unit_fix(&self, d: Diagnostic, l: &ast::Expr, r: &ast::Expr, a: &Unit, c: &Unit) -> Diagnostic {
        // A bare literal on one side may just lack its unit, but that is a guess about intent.
        if let AK::Float(_, None) = &r.kind {
            if c.is_dimensionless() {
                return d.fix1(
                    FixKind::ChangesMeaning,
                    format!("give the literal the unit {a}"),
                    r.span.at_end(),
                    format!("[{a}]"),
                );
            }
        }
        if let AK::Float(_, None) = &l.kind {
            if a.is_dimensionless() {
                return d.fix1(
                    FixKind::ChangesMeaning,
                    format!("give the literal the unit {c}"),
                    l.span.at_end(),
                    format!("[{c}]"),
                );
            }
        }
        d
    }

    fn struct_lit(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        name: &ast::Ident,
        fields: &[(ast::Ident, ast::Expr)],
    ) -> Expr {
        let Some(&sid) = self.struct_index.get(&name.name) else {
            let mut d = Diagnostic::new("E0200", name.span, format!("unknown struct `{}`", name.name));
            if let Some(s) = closest(&name.name, self.struct_index.keys()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), name.span, s);
            }
            self.err(d);
            for (_, v) in fields {
                self.expr(b, v);
            }
            return Self::err_expr();
        };
        let decl: Vec<(String, Ty)> = self.structs[sid].fields.clone();
        let def = self.type_spans.get(&name.name).copied();
        self.note_use(name.span, def);
        for (f, _) in fields {
            if let Some(i) = decl.iter().position(|(n, _)| *n == f.name) {
                let def = self.field_spans.get(&(sid, i)).copied();
                self.note_use(f.span, def);
            }
        }
        let mut ok = true;
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for (i, (f, _)) in fields.iter().enumerate() {
            if by_name.insert(f.name.as_str(), i).is_some() {
                self.err(Diagnostic::new("E0305", f.span, format!("field `{}` is given twice", f.name)));
                ok = false;
            }
            if !decl.iter().any(|(n, _)| *n == f.name) {
                self.field_index(sid, f);
                ok = false;
            }
        }
        let missing: Vec<&str> =
            decl.iter().filter(|(n, _)| !by_name.contains_key(n.as_str())).map(|(n, _)| n.as_str()).collect();
        if !missing.is_empty() {
            self.err(Diagnostic::new(
                "E0305",
                e.span,
                format!(
                    "struct `{}` literal is missing {}",
                    name.name,
                    missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ")
                ),
            ));
            ok = false;
        }
        let in_order = fields.iter().zip(decl.iter()).all(|((f, _), (n, _))| f.name == *n);
        if ok && !in_order {
            // Same set, wrong order: rewrite in declaration order.
            let text: Vec<String> = decl
                .iter()
                .map(|(n, _)| {
                    let (f, v) = &fields[by_name[n.as_str()]];
                    format!("{}: {}", f.name, self.text(v.span))
                })
                .collect();
            let inner = Span::new(fields[0].0.span.start, fields.last().unwrap().1.span.end);
            self.err(
                Diagnostic::new("E0305", e.span, format!("fields of `{}` must be in declaration order", name.name))
                    .note(
                        format!(
                            "declared order: {}",
                            decl.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
                        ),
                        None,
                    )
                    .fix1(FixKind::Mechanical, "reorder the fields", inner, text.join(", ")),
            );
            ok = false;
        }
        let mut vals = Vec::new();
        for (f, v) in fields {
            match decl.iter().find(|(n, _)| *n == f.name) {
                Some((_, t)) => vals.push(self.expect(b, v, &t.clone())),
                None => {
                    self.expr(b, v);
                }
            }
        }
        if !ok {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::StructLit(sid, vals), ty: Ty::Struct(sid) }
    }

    fn note_io(&mut self, b: &mut Body, span: Span, what: &str) {
        if b.in_contract {
            self.err(Diagnostic::new("E0390", span, format!("{what} performs io, which contracts cannot do")));
            return;
        }
        b.used_io = true;
        if !b.uses_io {
            self.err(Diagnostic::new("E0330", span, format!("{what} performs io, but this function is pure")).fix1(
                FixKind::ChangesMeaning,
                "declare the effect: uses io",
                b.uses_insert,
                " uses io",
            ));
        }
    }

    fn call(&mut self, b: &mut Body, e: &ast::Expr, callee: &ast::Ident, args: &[ast::Arg]) -> Expr {
        let name = callee.name.as_str();
        if BUILTINS.contains(&name) {
            return self.builtin(b, e, callee, args);
        }
        let Some(&fid) = self.func_index.get(name) else {
            let mut cands: Vec<String> = self.func_index.keys().cloned().collect();
            cands.extend(BUILTINS.iter().map(|s| s.to_string()));
            let mut d = Diagnostic::new("E0200", callee.span, format!("unknown function `{name}`"));
            if let Some(s) = closest(name, cands.iter()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), callee.span, s);
            }
            self.err(d);
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let params: Vec<(String, Ty)> = self.sigs[fid].params.clone();
        let ret = self.sigs[fid].ret.clone();
        self.note_use(callee.span, Some(self.sigs[fid].span));
        if self.sigs[fid].uses_io {
            self.note_io(b, callee.span, &format!("calling `{name}`"));
        }
        let sig_span = self.sigs[fid].span;
        let Some(ok) = self.check_args(name, e.span, args, &params, Some(sig_span)) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let mut vals = Vec::new();
        for (a, (_, t)) in args.iter().zip(&params) {
            vals.push(self.expect(b, &a.value, t));
        }
        if !ok {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::Call(fid, vals), ty: ret }
    }

    /// Argument count and naming rules shared by functions and enum variants. None when the
    /// count is wrong (arguments cannot be matched to parameters); otherwise whether names are right.
    fn check_args(
        &mut self,
        name: &str,
        call_span: Span,
        args: &[ast::Arg],
        params: &[(String, Ty)],
        sig_span: Option<Span>,
    ) -> Option<bool> {
        if args.len() != params.len() {
            let list = params.iter().map(|(n, t)| format!("{n}: {}", self.show(t))).collect::<Vec<_>>().join(", ");
            self.err(
                Diagnostic::new(
                    "E0303",
                    call_span,
                    format!("`{name}` takes {} argument(s), found {}", params.len(), args.len()),
                )
                .note(format!("signature: {name}({list})"), sig_span),
            );
            return None;
        }
        let e_span = call_span;
        let mut ok = true;
        if params.len() == 1 {
            if let Some(n) = &args[0].name {
                ok = false;
                self.err(
                    Diagnostic::new(
                        "E0304",
                        n.span,
                        format!("`{name}` has one parameter, so the argument is not named"),
                    )
                    .fix1(
                        FixKind::Mechanical,
                        "remove the name",
                        Span::new(n.span.start, args[0].value.span.start),
                        "",
                    ),
                );
            }
        } else if params.len() >= 2 {
            let unnamed: Vec<usize> = (0..args.len()).filter(|i| args[*i].name.is_none()).collect();
            if unnamed.len() == args.len() {
                ok = false;
                let edits = args
                    .iter()
                    .zip(params)
                    .map(|(a, (p, _))| Edit { span: a.value.span.at_start(), text: format!("{p}: ") })
                    .collect();
                self.err(Diagnostic::new("E0304", e_span, format!("calls to `{name}` name every argument")).fix(
                    FixKind::Mechanical,
                    "name the arguments in parameter order",
                    edits,
                ));
            } else if !unnamed.is_empty() {
                ok = false;
                let a = &args[unnamed[0]];
                self.err(
                    Diagnostic::new("E0304", a.value.span, format!("argument {} is not named", unnamed[0] + 1)).fix1(
                        FixKind::Mechanical,
                        format!("name it `{}`", params[unnamed[0]].0),
                        a.value.span.at_start(),
                        format!("{}: ", params[unnamed[0]].0),
                    ),
                );
            } else {
                let names: Vec<&str> = args.iter().map(|a| a.name.as_ref().unwrap().name.as_str()).collect();
                let pnames: Vec<&str> = params.iter().map(|(p, _)| p.as_str()).collect();
                for a in args {
                    let n = a.name.as_ref().unwrap();
                    if !pnames.contains(&n.name.as_str()) {
                        ok = false;
                        let mut d = Diagnostic::new("E0304", n.span, format!("`{name}` has no parameter `{}`", n.name))
                            .note(format!("parameters: {}", pnames.join(", ")), None);
                        let owned: Vec<String> = pnames.iter().map(|s| s.to_string()).collect();
                        if let Some(s) = closest(&n.name, owned.iter()) {
                            d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), n.span, s);
                        }
                        self.err(d);
                    }
                }
                let mut sorted_a = names.clone();
                sorted_a.sort();
                let mut sorted_p = pnames.clone();
                sorted_p.sort();
                if ok && sorted_a != sorted_p {
                    ok = false;
                    self.err(Diagnostic::new("E0304", e_span, "each parameter is named exactly once"));
                } else if ok && names != pnames {
                    ok = false;
                    let text: Vec<String> = pnames
                        .iter()
                        .map(|p| {
                            let a = args.iter().find(|a| a.name.as_ref().unwrap().name == *p).unwrap();
                            format!("{p}: {}", self.text(a.value.span))
                        })
                        .collect();
                    let inner =
                        Span::new(args[0].name.as_ref().unwrap().span.start, args.last().unwrap().value.span.end);
                    self.err(
                        Diagnostic::new("E0304", e_span, "named arguments must be in parameter order")
                            .note(format!("parameter order: {}", pnames.join(", ")), None)
                            .fix1(FixKind::Mechanical, "reorder the arguments", inner, text.join(", ")),
                    );
                }
            }
        }
        Some(ok)
    }

    fn builtin(&mut self, b: &mut Body, e: &ast::Expr, callee: &ast::Ident, args: &[ast::Arg]) -> Expr {
        let name = callee.name.as_str();
        for a in args {
            if let Some(n) = &a.name {
                self.err(
                    Diagnostic::new("E0304", n.span, format!("built-in `{name}` takes positional arguments")).fix1(
                        FixKind::Mechanical,
                        "remove the name",
                        Span::new(n.span.start, a.value.span.start),
                        "",
                    ),
                );
            }
        }
        if name == "print" {
            // Recorded first, so an error in the argument cannot also report `uses io` as unused.
            self.note_io(b, callee.span, "print");
        }
        let vals: Vec<Expr> = args.iter().map(|a| self.expr(b, &a.value)).collect();
        if vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        if name == "print" {
            if vals.len() != 1 {
                let mut d = Diagnostic::new("E0303", e.span, format!("print takes one argument, found {}", vals.len()))
                    .note("build the line with interpolation: print(\"x = {x}\")", None);
                if let Some(text) = self.as_interpolation(args) {
                    let inner = Span::new(args[0].value.span.start, args.last().unwrap().value.span.end);
                    d = d.fix1(FixKind::Mechanical, "join the arguments into one interpolated string", inner, text);
                }
                self.err(d);
                return Self::err_expr();
            }
            if !printable(&vals[0].ty) {
                let t = self.show(&vals[0].ty);
                let hint = match vals[0].ty {
                    Ty::Struct(_) => "print the fields one by one",
                    Ty::Array(_) => "print the elements in a loop",
                    Ty::Enum(_) => "match on it and print each case",
                    _ => "only values have text",
                };
                self.err(
                    Diagnostic::new("E0407", args[0].value.span, format!("print takes i64, f64, bool or str, not {t}"))
                        .note(hint, None),
                );
                return Self::err_expr();
            }
            return Expr { kind: ExprKind::Builtin(Builtin::Print, vals), ty: Ty::Void };
        }
        if vals.len() != 1 {
            self.err(Diagnostic::new("E0303", e.span, format!("`{name}` takes 1 argument, found {}", vals.len())));
            return Self::err_expr();
        }
        let arg = &args[0].value;
        let site = self.site(e.span);
        let x = vals.into_iter().next().unwrap();
        let t = x.ty.clone();
        let bad = |c: &mut Self, want: &str| {
            let got = c.show(&t);
            c.err(Diagnostic::new("E0300", arg.span, format!("`{name}` takes {want}, found {got}")));
            Self::err_expr()
        };
        match name {
            "assert" => {
                if t != Ty::Bool {
                    return bad(self, "bool");
                }
                let message = format!("assertion failed at {site}: {}", self.text(arg.span));
                Expr { kind: ExprKind::Builtin(Builtin::Assert { message }, vec![x]), ty: Ty::Void }
            }
            "sqrt" => match &t {
                Ty::F64(u) => match u.sqrt() {
                    Some(r) => Expr { kind: ExprKind::Builtin(Builtin::Sqrt, vec![x]), ty: Ty::F64(r) },
                    None => {
                        let t = self.show(&t);
                        self.err(Diagnostic::new("E0360", arg.span, format!("sqrt of {t} has no unit")));
                        Self::err_expr()
                    }
                },
                _ => bad(self, "f64"),
            },
            "abs" => match &t {
                Ty::I64 => Expr {
                    kind: ExprKind::Builtin(Builtin::AbsI64 { message: overflow_message("abs", &site) }, vec![x]),
                    ty: Ty::I64,
                },
                Ty::F64(_) => Expr { kind: ExprKind::Builtin(Builtin::AbsF64, vec![x]), ty: t.clone() },
                _ => bad(self, "i64 or f64"),
            },
            "to_f64" => {
                if t != Ty::I64 {
                    return bad(self, "i64");
                }
                Expr { kind: ExprKind::Builtin(Builtin::ToF64, vec![x]), ty: Ty::F64(Unit::dimensionless()) }
            }
            "to_i64" => match &t {
                Ty::F64(u) if u.is_dimensionless() => Expr {
                    kind: ExprKind::Builtin(
                        Builtin::ToI64 { message: format!("to_i64 of a non-finite or out-of-range value at {site}") },
                        vec![x],
                    ),
                    ty: Ty::I64,
                },
                Ty::F64(u) => {
                    let t = self.show(&t);
                    self.err(
                        Diagnostic::new("E0301", arg.span, format!("to_i64 takes a dimensionless f64, found {t}"))
                            .fix1(
                                FixKind::ChangesMeaning,
                                format!("divide by 1.0[{u}] to state the unit you are counting in"),
                                arg.span.at_end(),
                                format!(" / 1.0[{u}]"),
                            ),
                    );
                    Self::err_expr()
                }
                _ => bad(self, "f64"),
            },
            _ => unreachable!("builtin {name}"),
        }
    }
}

impl<'a> Checker<'a> {
    /// `print(a, " ", b)` as `"{a} {b}"`, when every argument can be written inside a string.
    fn as_interpolation(&self, args: &[ast::Arg]) -> Option<String> {
        let mut out = String::from("\"");
        for a in args {
            let text = self.text(a.value.span);
            match &a.value.kind {
                AK::Str(_) | AK::Interp(_) => {
                    let inner = &text[1..text.len() - 1];
                    if matches!(a.value.kind, AK::Str(_)) {
                        out.push_str(&inner.replace('{', "{{").replace('}', "}}"));
                    } else {
                        out.push_str(inner);
                    }
                }
                _ => {
                    if text.contains(['"', '{', '}', '\n']) || a.name.is_some() {
                        return None;
                    }
                    out.push('{');
                    out.push_str(&text);
                    out.push('}');
                }
            }
        }
        out.push('"');
        Some(out)
    }

    /// The enum named by `base` in `Shape.Circle`, unless a local has that name.
    fn enum_name(&self, b: &Body, base: &ast::Expr) -> Option<EnumId> {
        let AK::Name(n) = &base.kind else { return None };
        if self.lookup_local(b, n).is_some() {
            return None;
        }
        self.enum_index.get(&n.name).copied()
    }

    /// `Shape.Circle(r)` or `Shape.Empty` (args None).
    fn variant_ctor(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        (enum_id, enum_span): (EnumId, Span),
        vname: &ast::Ident,
        args: Option<&[ast::Arg]>,
    ) -> Expr {
        let ename = self.enums[enum_id].name.clone();
        let def = self.type_spans.get(&ename).copied();
        self.note_use(enum_span, def);
        let variants = self.enums[enum_id].variants.clone();
        let Some(vi) = variants.iter().position(|v| v.name == vname.name) else {
            let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
            let mut d = Diagnostic::new("E0200", vname.span, format!("`{ename}` has no variant `{}`", vname.name))
                .note(format!("variants: {}", names.join(", ")), None);
            if let Some(s) = closest(&vname.name, names.iter()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), vname.span, s);
            }
            self.err(d);
            for a in args.unwrap_or(&[]) {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let fields = variants[vi].fields.clone();
        let full = format!("{ename}.{}", vname.name);
        let def = self.variant_spans.get(&(enum_id, vi)).copied();
        self.note_use(vname.span, def);
        let Some(args) = args else {
            if fields.is_empty() {
                return Expr {
                    kind: ExprKind::Variant { enum_id, variant: vi, fields: Vec::new() },
                    ty: Ty::Enum(enum_id),
                };
            }
            let list = fields.iter().map(|(n, t)| format!("{n}: {}", self.show(t))).collect::<Vec<_>>().join(", ");
            self.err(Diagnostic::new("E0303", e.span, format!("`{full}` has fields; construct it as {full}({list})")));
            return Self::err_expr();
        };
        if fields.is_empty() {
            let mut d = Diagnostic::new("E0303", e.span, format!("`{full}` has no fields"));
            if args.is_empty() {
                d = d.fix1(FixKind::Mechanical, "remove the parentheses", Span::new(vname.span.end, e.span.end), "");
            }
            self.err(d);
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        let Some(ok) = self.check_args(&full, e.span, args, &fields, None) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let vals: Vec<Expr> = args.iter().zip(&fields).map(|(a, (_, t))| self.expect(b, &a.value, t)).collect();
        if !ok {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::Variant { enum_id, variant: vi, fields: vals }, ty: Ty::Enum(enum_id) }
    }

    /// `Some(x)`, `None`, `Ok(x)`, `Err(e)`. Only `Some` can be typed without an expected type.
    fn builtin_ctor(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        callee: &ast::Ident,
        args: Option<&[ast::Arg]>,
        hint: Option<&Ty>,
    ) -> Expr {
        let name = callee.name.as_str();
        let kind = if name == "Some" || name == "None" { "Option" } else { "Result" };
        let hinted = match hint {
            Some(Ty::Enum(id)) if self.builtin_kind(*id) == Some(kind) => Some(*id),
            _ => None,
        };
        let variant = match name {
            "Some" => OPTION_SOME,
            "None" => OPTION_NONE,
            "Ok" => RESULT_OK,
            _ => RESULT_ERR,
        };
        // The value, if this constructor takes one.
        let arg = if name == "None" {
            if let Some(args) = args {
                let mut d = Diagnostic::new("E0303", e.span, "None takes no value");
                if args.is_empty() {
                    d = d.fix1(
                        FixKind::Mechanical,
                        "remove the parentheses",
                        Span::new(callee.span.end, e.span.end),
                        "",
                    );
                }
                self.err(d);
                return Self::err_expr();
            }
            None
        } else {
            let args = args.unwrap_or(&[]);
            if args.len() != 1 {
                self.err(Diagnostic::new("E0303", e.span, format!("`{name}` takes one value: {name}(x)")));
                for a in args {
                    self.expr(b, &a.value);
                }
                return Self::err_expr();
            }
            if let Some(n) = &args[0].name {
                self.err(Diagnostic::new("E0304", n.span, format!("`{name}` takes its value without a name")).fix1(
                    FixKind::Mechanical,
                    "remove the name",
                    Span::new(n.span.start, args[0].value.span.start),
                    "",
                ));
                return Self::err_expr();
            }
            Some(&args[0].value)
        };
        let enum_id = match hinted {
            Some(id) => id,
            None if name == "Some" => {
                let x = self.expr(b, arg.unwrap());
                if x.ty == Ty::Error {
                    return Self::err_expr();
                }
                let Ty::Enum(id) = self.option_of(x.ty.clone()) else { unreachable!() };
                return Expr { kind: ExprKind::Variant { enum_id: id, variant, fields: vec![x] }, ty: Ty::Enum(id) };
            }
            None => {
                if let Some(a) = arg {
                    self.expr(b, a);
                }
                match hint {
                    Some(t) if *t != Ty::Error => {
                        let t = self.show(t);
                        self.err(Diagnostic::new("E0300", e.span, format!("expected {t}, found {kind}")));
                    }
                    Some(_) => {}
                    None => {
                        let example = if kind == "Option" {
                            "let x: Option[i64] = None;"
                        } else {
                            "let r: Result[i64, str] = Err(\"bad input\");"
                        };
                        self.err(
                            Diagnostic::new("E0404", e.span, format!("the type of `{name}` cannot be inferred here"))
                                .note(format!("state the type: {example}"), None),
                        );
                    }
                }
                return Self::err_expr();
            }
        };
        let fields = match arg {
            None => Vec::new(),
            Some(a) => {
                let t = self.enums[enum_id].variants[variant].fields[0].1.clone();
                vec![self.expect(b, a, &t)]
            }
        };
        Expr { kind: ExprKind::Variant { enum_id, variant, fields }, ty: Ty::Enum(enum_id) }
    }

    fn method(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        receiver: &ast::Expr,
        method: &ast::Ident,
        args: &[ast::Arg],
    ) -> Expr {
        if let Some(eid) = self.enum_name(b, receiver) {
            return self.variant_ctor(b, e, (eid, receiver.span), method, Some(args));
        }
        let m = method.name.as_str();
        for a in args {
            if let Some(n) = &a.name {
                self.err(Diagnostic::new("E0304", n.span, format!("`{m}` takes positional arguments")).fix1(
                    FixKind::Mechanical,
                    "remove the name",
                    Span::new(n.span.start, a.value.span.start),
                    "",
                ));
            }
        }
        let want_args = match m {
            "push" => 1,
            _ => 0,
        };
        if matches!(m, "push" | "pop" | "len") && args.len() != want_args {
            self.err(Diagnostic::new(
                "E0303",
                e.span,
                format!("`{m}` takes {want_args} argument(s), found {}", args.len()),
            ));
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        if m == "push" || m == "pop" {
            if !is_place(receiver) {
                let x = self.expr(b, receiver);
                if x.ty != Ty::Error {
                    self.err(
                        Diagnostic::new(
                            "E0406",
                            receiver.span,
                            format!("`{m}` changes an array in place, so it needs a var"),
                        )
                        .note("bind the array first: var xs: [i64] = make(); xs.push(1);", None),
                    );
                }
                return Self::err_expr();
            }
            let Some((local, path, ty)) = self.place(b, receiver) else {
                return Self::err_expr();
            };
            let elem = match ty {
                Ty::Array(t) => *t,
                Ty::Error => return Self::err_expr(),
                other => {
                    let t = self.show(&other);
                    self.err(Diagnostic::new(
                        "E0406",
                        method.span,
                        format!("`{m}` is an array method; {t} has no `{m}`"),
                    ));
                    return Self::err_expr();
                }
            };
            if m == "pop" {
                // pop reads the array; push and assignment only write it.
                b.meta[local].used = true;
            }
            if m == "push" {
                let v = self.expect(b, &args[0].value, &elem);
                return Expr { kind: ExprKind::Push { local, path, value: Box::new(v) }, ty: Ty::Void };
            }
            let message = pop_message(&self.site(e.span));
            return Expr { kind: ExprKind::Pop { local, path, message }, ty: elem };
        }
        let x = self.expr(b, receiver);
        for a in args {
            self.expr(b, &a.value);
        }
        if x.ty == Ty::Error {
            return Self::err_expr();
        }
        if m == "len" && matches!(x.ty, Ty::Str | Ty::Array(_)) {
            return Expr { kind: ExprKind::Builtin(Builtin::Len, vec![x]), ty: Ty::I64 };
        }
        let t = self.show(&x.ty);
        let have = match x.ty {
            Ty::Array(_) => "arrays have len, push and pop",
            Ty::Str => "str has len",
            _ => "only arrays and str have methods; write a function that takes the value",
        };
        self.err(Diagnostic::new("E0406", method.span, format!("{t} has no method `{m}`")).note(have, None));
        Self::err_expr()
    }
}

/// Syntactically a variable, a field of one or an element of one.
fn is_place(e: &ast::Expr) -> bool {
    match &e.kind {
        AK::Name(_) => true,
        AK::Field(b, _) | AK::Index(b, _) | AK::Paren(b) => is_place(b),
        _ => false,
    }
}

fn cmp_op(op: BinOp) -> CmpOp {
    match op {
        BinOp::Eq => CmpOp::Eq,
        BinOp::Ne => CmpOp::Ne,
        BinOp::Lt => CmpOp::Lt,
        BinOp::Le => CmpOp::Le,
        BinOp::Gt => CmpOp::Gt,
        BinOp::Ge => CmpOp::Ge,
        _ => unreachable!("{op:?} is not a comparison"),
    }
}

/// Conservative "every path returns": a return, or an if/else whose branches all return.
fn always_returns(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(_) => true,
        StmtKind::Match { arms, .. } => !arms.is_empty() && arms.iter().all(|a| always_returns(&a.body.stmts)),
        StmtKind::If { then, else_: Some(e), .. } => {
            always_returns(&then.stmts)
                && match e.as_ref() {
                    Else::Block(b) => always_returns(&b.stmts),
                    Else::If(st) => always_returns(std::slice::from_ref(st)),
                }
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::{closest, distance};

    #[test]
    fn suggestions() {
        assert_eq!(distance("cuont", "count"), 1);
        assert_eq!(distance("speed", "sped"), 1);
        assert_eq!(distance("abc", "xyz"), 3);
        let names = ["count".to_string(), "total".to_string()];
        assert_eq!(closest("cuont", names.iter()), Some("count".to_string()));
        assert_eq!(closest("zzz", names.iter()), None);
    }
}
