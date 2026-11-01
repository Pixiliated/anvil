//! Generic functions, structs and enums, made concrete by instantiation: every use with new type
//! arguments gets its own function or type in the IR, so the backends never see a type
//! parameter.
//!
//! A generic body is checked once against its constraints, with each parameter as `Ty::Param`;
//! that check reports the body's errors. Calls infer the type arguments from the arguments (every
//! parameter appears in a parameter type), check the constraints at the call, and instantiate
//! the body for those types, quietly; an error there (the constraints let something through) is
//! reported in the generic's body with a note naming the instance and where it was used.

use super::{Body, Checker, Def, FuncSig, Loc};
use crate::ast;
use crate::diag::{Diagnostic, FixKind};
use crate::ir::*;
use crate::span::{SourceFile, Span};
use std::collections::HashMap;

/// What a type parameter may do beyond being copied, stored, passed and compared with `==`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Constraint {
    Ordered,
    Text,
    Key,
}

pub(super) const CONSTRAINTS: &[(&str, Constraint, &str)] = &[
    ("ordered", Constraint::Ordered, "i64, f64 and str, so <, <=, > and >= work"),
    ("text", Constraint::Text, "i64, f64, bool and str, so print and interpolation work"),
    ("key", Constraint::Key, "i64 and str, so it can be a map key"),
];

fn constraint_name(c: Constraint) -> &'static str {
    CONSTRAINTS.iter().find(|(_, k, _)| *k == c).unwrap().0
}

#[derive(Clone)]
pub(super) struct TypeParam {
    pub name: String,
    pub constraints: Vec<Constraint>,
    /// Where `: c` or ` + c` would go, for fixes that add a constraint.
    pub insert: Span,
    pub has_constraints: bool,
}

#[derive(Clone, Copy)]
pub(super) enum GenericKind<'a> {
    Fn(&'a ast::FnDecl),
    Struct(&'a ast::StructDecl),
    Enum(&'a ast::EnumDecl),
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Shape {
    Unresolved,
    Resolving,
    Ready,
}

pub(super) struct Generic<'a> {
    pub name: String,
    pub kind: GenericKind<'a>,
    pub params: Vec<TypeParam>,
    pub module: usize,
    pub public: bool,
    pub shape: Shape,
    /// The abstract signature (functions): parameters, return type, effects.
    pub sig: (Vec<(String, Ty)>, Ty, Effects),
    /// The abstract fields (structs) or variants (enums), with `Ty::Param` for parameters.
    pub fields: Vec<(String, Ty)>,
    pub variants: Vec<VariantDef>,
    /// Instances made before the shape was known, filled when it is.
    pub unfilled: Vec<(usize, Vec<Ty>)>,
}

/// A function instance whose body is checked at the end of the module.
pub(super) struct Pending {
    pub id: FuncId,
    pub generic: usize,
    pub args: Vec<Ty>,
    pub site: (usize, Span),
    /// 1 for an instance asked for by ordinary code, one more for each instance asking for another.
    pub level: usize,
}

/// The name tables of one module, kept so an instance can be checked where its generic lives.
#[derive(Clone, Default)]
pub(super) struct Scope {
    pub func_index: HashMap<String, FuncId>,
    pub struct_index: HashMap<String, StructId>,
    pub enum_index: HashMap<String, EnumId>,
    pub units: HashMap<String, crate::units::Unit>,
    pub type_spans: HashMap<String, Loc>,
    pub unit_spans: HashMap<String, Loc>,
    pub generic_fns: HashMap<String, usize>,
    pub generic_types: HashMap<String, usize>,
    pub imported: HashMap<String, (Span, usize)>,
    pub failed_imports: std::collections::HashSet<String>,
}

/// Instances deeper than this are polymorphic recursion (`f[T]` calling `f[[T]]`), never ending.
const MAX_DEPTH: usize = 32;

impl<'a> Checker<'a> {
    // ---- declarations ----

    /// Type parameters of a declaration; E0201 for a name clash, E0702 for an unknown constraint.
    pub(super) fn type_params(&mut self, decls: &[ast::TypeParamDecl]) -> Vec<TypeParam> {
        let mut out: Vec<TypeParam> = Vec::new();
        for d in decls {
            let n = &d.name.name;
            if super::TYPE_NAMES.contains(&n.as_str())
                || self.struct_index.contains_key(n)
                || self.enum_index.contains_key(n)
                || out.iter().any(|p| p.name == *n)
            {
                self.err(Diagnostic::new(
                    "E0201",
                    d.name.span,
                    format!("type parameter `{n}` reuses the name of a type"),
                ));
            }
            let mut constraints = Vec::new();
            for c in &d.constraints {
                match CONSTRAINTS.iter().find(|(name, _, _)| *name == c.name) {
                    Some((_, k, _)) => constraints.push(*k),
                    None => {
                        let names: Vec<String> = CONSTRAINTS.iter().map(|(n, _, _)| n.to_string()).collect();
                        let mut diag = Diagnostic::new("E0702", c.span, format!("unknown constraint `{}`", c.name))
                            .note("the constraints are ordered, text and key", None);
                        if let Some(s) = super::closest(&c.name, names.iter()) {
                            diag = diag.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), c.span, s);
                        }
                        self.err(diag);
                    }
                }
            }
            let insert = d.constraints.last().map_or(d.name.span, |c| c.span).at_end();
            out.push(TypeParam { name: n.clone(), constraints, insert, has_constraints: !d.constraints.is_empty() });
        }
        out
    }

    pub(super) fn add_generic(
        &mut self,
        kind: GenericKind<'a>,
        name: &ast::Ident,
        params: Vec<TypeParam>,
        public: bool,
    ) {
        let id = self.generics.len();
        self.generics.push(Generic {
            name: name.name.clone(),
            kind,
            params,
            module: self.module,
            public,
            shape: Shape::Unresolved,
            sig: (Vec::new(), Ty::Void, Effects::default()),
            fields: Vec::new(),
            variants: Vec::new(),
            unfilled: Vec::new(),
        });
        match kind {
            GenericKind::Fn(_) => self.generic_fns.insert(name.name.clone(), id),
            _ => self.generic_types.insert(name.name.clone(), id),
        };
    }

    /// Runs `f` with the type parameters of generic `g` standing for themselves (`Ty::Param`).
    fn as_params<T>(&mut self, g: usize, f: impl FnOnce(&mut Self) -> T) -> T {
        let params = self.generics[g].params.clone();
        let saved = (std::mem::take(&mut self.type_params), std::mem::take(&mut self.param_constraints));
        for p in &params {
            self.type_params.insert(p.name.clone(), Ty::Param(p.name.clone()));
            self.param_constraints.insert(p.name.clone(), p.clone());
        }
        let out = f(self);
        (self.type_params, self.param_constraints) = saved;
        out
    }

    /// Resolves a generic's abstract signature, fields or variants (once), then fills the instances
    /// made of it before. E0700 when a type parameter appears nowhere callers can infer it from.
    pub(super) fn resolve_shape(&mut self, g: usize) {
        if self.generics[g].shape != Shape::Unresolved {
            return;
        }
        self.generics[g].shape = Shape::Resolving;
        let kind = self.generics[g].kind;
        let mut used: Vec<Ty> = Vec::new();
        let where_ = match kind {
            GenericKind::Fn(d) => {
                let sig = self.as_params(g, |c| {
                    let params: Vec<(String, Ty)> =
                        d.params.iter().map(|p| (p.name.name.clone(), c.resolve_type(&p.ty))).collect();
                    let ret = d.ret.as_ref().map(|t| c.resolve_type(t)).unwrap_or(Ty::Void);
                    (params, ret, c.effects(&d.uses))
                });
                used.extend(sig.0.iter().map(|(_, t)| t.clone()));
                self.generics[g].sig = sig;
                "parameter's type"
            }
            GenericKind::Struct(d) => {
                let fields = self.as_params(g, |c| {
                    d.fields.iter().map(|(f, t)| (f.name.clone(), c.resolve_type(t))).collect::<Vec<_>>()
                });
                used.extend(fields.iter().map(|(_, t)| t.clone()));
                self.generics[g].fields = fields;
                "field's type"
            }
            GenericKind::Enum(d) => {
                let variants = self.as_params(g, |c| {
                    d.variants
                        .iter()
                        .map(|v| VariantDef {
                            name: v.name.name.clone(),
                            fields: v.fields.iter().map(|(f, t)| (f.name.clone(), c.resolve_type(t))).collect(),
                        })
                        .collect::<Vec<_>>()
                });
                used.extend(variants.iter().flat_map(|v| v.fields.iter().map(|(_, t)| t.clone())));
                self.generics[g].variants = variants;
                "variant field's type"
            }
        };
        let decls = match kind {
            GenericKind::Fn(d) => &d.type_params,
            GenericKind::Struct(d) => &d.type_params,
            GenericKind::Enum(d) => &d.type_params,
        };
        for p in decls {
            if !used.iter().any(|t| self.mentions(t, &p.name.name)) {
                self.err(
                    Diagnostic::new("E0700", p.name.span, format!("type parameter `{}` appears in no {where_}", p.name.name))
                        .note("type arguments are never written; they are read from what is passed, so each parameter must appear there", None),
                );
            }
        }
        self.generics[g].shape = Shape::Ready;
        self.generic_member_defs(g);
        for (id, args) in std::mem::take(&mut self.generics[g].unfilled) {
            self.fill(g, id, &args);
        }
    }

    /// Whether `t` mentions type parameter `name`, through arrays, maps and instances.
    fn mentions(&self, t: &Ty, name: &str) -> bool {
        match t {
            Ty::Param(n) => n == name,
            Ty::Array(e) => self.mentions(e, name),
            Ty::Map(k, v) => self.mentions(k, name) || self.mentions(v, name),
            Ty::Struct(id) => self.struct_inst.get(id).is_some_and(|(_, a)| a.iter().any(|t| self.mentions(t, name))),
            Ty::Enum(id) => match (self.enum_inst.get(id), self.builtin_args.get(id)) {
                (Some((_, a)), _) | (_, Some(a)) => a.iter().any(|t| self.mentions(t, name)),
                _ => false,
            },
            _ => false,
        }
    }

    /// E0308 for a generic struct or enum that contains itself by value (`Box[T] { inner: Box[T] }`).
    pub(super) fn check_generic_type(&mut self, g: usize) {
        let params: Vec<Ty> = self.generics[g].params.iter().map(|p| Ty::Param(p.name.clone())).collect();
        let me = self.as_params(g, |c| c.instantiate_type(g, params));
        if me != Ty::Error && self.contains_by_value(&me, &me, &mut Vec::new()) {
            let (span, kind) = match self.generics[g].kind {
                GenericKind::Struct(d) => (d.name.span, "struct"),
                GenericKind::Enum(d) => (d.name.span, "enum"),
                GenericKind::Fn(_) => return,
            };
            let name = self.generics[g].name.clone();
            self.err(
                Diagnostic::new("E0308", span, format!("{kind} `{name}` contains itself"))
                    .note("put the inner values in an array", None),
            );
        }
    }

    /// The types a generic shows to its users: parameter and return types, or field types.
    pub(super) fn shown_types(&self, g: usize) -> Vec<Ty> {
        let gen = &self.generics[g];
        match gen.kind {
            GenericKind::Fn(_) => gen.sig.0.iter().map(|(_, t)| t.clone()).chain([gen.sig.1.clone()]).collect(),
            GenericKind::Struct(_) => gen.fields.iter().map(|(_, t)| t.clone()).collect(),
            GenericKind::Enum(_) => gen.variants.iter().flat_map(|v| v.fields.iter().map(|(_, t)| t.clone())).collect(),
        }
    }

    // ---- types ----

    /// `Pair[i64, str]`: arity and constraints checked here, at the use.
    pub(super) fn generic_type(&mut self, g: usize, t: &ast::TypeExpr, args: &[ast::TypeExpr]) -> Ty {
        let (name, want) = (self.generics[g].name.clone(), self.generics[g].params.len());
        if args.len() != want {
            let shape = self.generics[g].params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
            self.err(Diagnostic::new(
                "E0307",
                t.span,
                format!("{name} takes {want} type argument(s): {name}[{shape}]"),
            ));
            return Ty::Error;
        }
        let tys: Vec<Ty> = args.iter().map(|a| self.resolve_type(a)).collect();
        if tys.contains(&Ty::Error) {
            return Ty::Error;
        }
        if !self.constraints_hold(g, &tys, t.span) {
            return Ty::Error;
        }
        self.instantiate_type(g, tys)
    }

    /// The struct or enum instance of generic type `g` for `args`, made on first use.
    pub(super) fn instantiate_type(&mut self, g: usize, args: Vec<Ty>) -> Ty {
        let key = (g, format!("{args:?}"));
        let is_struct = matches!(self.generics[g].kind, GenericKind::Struct(_));
        if let Some(&id) = self.instances.get(&key) {
            return if is_struct { Ty::Struct(id) } else { Ty::Enum(id) };
        }
        if self.depth >= MAX_DEPTH {
            if !self.depth_reported {
                self.depth_reported = true;
                let span = match self.generics[g].kind {
                    GenericKind::Fn(d) => d.name.span,
                    GenericKind::Struct(d) => d.name.span,
                    GenericKind::Enum(d) => d.name.span,
                };
                let name = self.generics[g].name.clone();
                self.err(Diagnostic::new(
                    "E0704",
                    span,
                    format!("`{name}` instantiates itself with ever larger types"),
                ));
            }
            return Ty::Error;
        }
        let shown: Vec<String> = args.iter().map(|a| self.show(a)).collect();
        let name = format!("{}[{}]", self.generics[g].name, shown.join(", "));
        let owner = (self.generics[g].module, self.generics[g].public);
        let id = if is_struct {
            let id = self.structs.len();
            self.structs.push(StructDef { name, fields: Vec::new() });
            self.struct_owner.push(owner);
            self.struct_inst.insert(id, (g, args.clone()));
            id
        } else {
            let id = self.enums.len();
            self.enums.push(EnumDef { name, variants: Vec::new() });
            self.enum_owner.push(Some(owner));
            self.enum_inst.insert(id, (g, args.clone()));
            id
        };
        self.instances.insert(key, id);
        if self.generics[g].shape == Shape::Ready {
            self.fill(g, id, &args);
        } else {
            self.generics[g].unfilled.push((id, args));
            if self.generics[g].shape == Shape::Unresolved && self.generics[g].module == self.module {
                self.resolve_shape(g);
            }
        }
        if is_struct {
            Ty::Struct(id)
        } else {
            Ty::Enum(id)
        }
    }

    /// Where generic `g` is declared.
    pub(super) fn generic_loc(&self, g: usize) -> Loc {
        let span = match self.generics[g].kind {
            GenericKind::Fn(d) => d.name.span,
            GenericKind::Struct(d) => d.name.span,
            GenericKind::Enum(d) => d.name.span,
        };
        Loc { file: self.generics[g].module, span }
    }

    /// Facts for the fields and variants of generic type `g`, once its shape is known. Shapes
    /// are resolved once, possibly while another module is checked, so these are recorded here
    /// with the generic's own file.
    fn generic_member_defs(&mut self, g: usize) {
        let file = self.generics[g].module;
        let name = self.generics[g].name.clone();
        let mut defs = Vec::new();
        match self.generics[g].kind {
            GenericKind::Fn(_) => {}
            GenericKind::Struct(d) => {
                for ((f, _), (_, ty)) in d.fields.iter().zip(&self.generics[g].fields) {
                    defs.push((f.span, format!("{name}.{}", f.name), "field", self.show(ty)));
                }
            }
            GenericKind::Enum(d) => {
                for (v, def) in d.variants.iter().zip(&self.generics[g].variants) {
                    let vname = format!("{name}.{}", v.name.name);
                    let shown: Vec<String> = def.fields.iter().map(|(n, t)| format!("{n}: {}", self.show(t))).collect();
                    let text =
                        if shown.is_empty() { def.name.clone() } else { format!("{}({})", def.name, shown.join(", ")) };
                    defs.push((v.name.span, vname.clone(), "variant", text));
                    for ((f, _), (_, ty)) in v.fields.iter().zip(&def.fields) {
                        defs.push((f.span, format!("{vname}.{}", f.name), "field", self.show(ty)));
                    }
                }
            }
        }
        for (span, name, kind, detail) in defs {
            self.facts.defs.push(Def { file, span, name, kind, detail, scope: None });
        }
    }

    /// Gives instance `id` of generic type `g` its fields or variants for `args`.
    fn fill(&mut self, g: usize, id: usize, args: &[Ty]) {
        // Uses of an instance's members lead to the generic's declaration of them.
        let file = self.generics[g].module;
        match self.generics[g].kind {
            GenericKind::Fn(_) => {}
            GenericKind::Struct(d) => {
                for (i, (f, _)) in d.fields.iter().enumerate() {
                    self.field_spans.insert((id, i), Loc { file, span: f.span });
                }
            }
            GenericKind::Enum(d) => {
                for (vi, v) in d.variants.iter().enumerate() {
                    self.variant_spans.insert((id, vi), Loc { file, span: v.name.span });
                    for (fi, (f, _)) in v.fields.iter().enumerate() {
                        self.variant_field_spans.insert((id, vi, fi), Loc { file, span: f.span });
                    }
                }
            }
        }
        let map = self.binding(g, args);
        self.depth += 1;
        match self.generics[g].kind {
            GenericKind::Struct(_) => {
                let fields: Vec<(String, Ty)> = self.generics[g].fields.clone();
                let fields = fields.into_iter().map(|(n, t)| (n, self.subst(&t, &map))).collect();
                self.structs[id].fields = fields;
            }
            _ => {
                let variants: Vec<VariantDef> = self.generics[g].variants.clone();
                let variants = variants
                    .into_iter()
                    .map(|v| VariantDef {
                        name: v.name,
                        fields: v.fields.into_iter().map(|(n, t)| (n, self.subst(&t, &map))).collect(),
                    })
                    .collect();
                self.enums[id].variants = variants;
            }
        }
        self.depth -= 1;
    }

    fn binding(&self, g: usize, args: &[Ty]) -> HashMap<String, Ty> {
        self.generics[g].params.iter().map(|p| p.name.clone()).zip(args.iter().cloned()).collect()
    }

    /// `t` with each parameter in `map` replaced by its type, instantiating what that needs.
    pub(super) fn subst(&mut self, t: &Ty, map: &HashMap<String, Ty>) -> Ty {
        match t {
            Ty::Param(n) => map.get(n).cloned().unwrap_or_else(|| t.clone()),
            Ty::Array(e) => Ty::Array(Box::new(self.subst(e, map))),
            Ty::Map(k, v) => Ty::Map(Box::new(self.subst(k, map)), Box::new(self.subst(v, map))),
            Ty::Struct(id) => match self.struct_inst.get(id).cloned() {
                Some((g, args)) => {
                    let args: Vec<Ty> = args.iter().map(|a| self.subst(a, map)).collect();
                    self.instantiate_type(g, args)
                }
                None => t.clone(),
            },
            Ty::Enum(id) => {
                if let Some((g, args)) = self.enum_inst.get(id).cloned() {
                    let args: Vec<Ty> = args.iter().map(|a| self.subst(a, map)).collect();
                    return self.instantiate_type(g, args);
                }
                match self.builtin_args.get(id).cloned() {
                    Some(args) if args.len() == 1 => {
                        let a = self.subst(&args[0], map);
                        self.option_of(a)
                    }
                    Some(args) => {
                        let (ok, err) = (self.subst(&args[0], map), self.subst(&args[1], map));
                        self.result_of(ok, err)
                    }
                    None => t.clone(),
                }
            }
            _ => t.clone(),
        }
    }

    /// Binds the parameters in `pattern` so it matches `actual`; the first binding of each wins
    /// (a conflicting later argument is then reported as a plain type mismatch).
    fn unify(&self, pattern: &Ty, actual: &Ty, binds: &mut HashMap<String, Ty>) {
        match (pattern, actual) {
            (Ty::Param(n), a) if *a != Ty::Error => {
                binds.entry(n.clone()).or_insert_with(|| a.clone());
            }
            (Ty::Array(p), Ty::Array(a)) => self.unify(p, a, binds),
            (Ty::Map(pk, pv), Ty::Map(ak, av)) => {
                self.unify(pk, ak, binds);
                self.unify(pv, av, binds);
            }
            (Ty::Struct(p), Ty::Struct(a)) => {
                if let (Some((pg, pargs)), Some((ag, aargs))) = (self.struct_inst.get(p), self.struct_inst.get(a)) {
                    if pg == ag {
                        for (x, y) in pargs.iter().zip(aargs) {
                            self.unify(x, y, binds);
                        }
                    }
                }
            }
            (Ty::Enum(p), Ty::Enum(a)) => {
                let pa = self
                    .enum_inst
                    .get(p)
                    .map(|(g, a)| (Some(*g), a))
                    .or_else(|| self.builtin_args.get(p).map(|a| (None, a)));
                let aa = self
                    .enum_inst
                    .get(a)
                    .map(|(g, a)| (Some(*g), a))
                    .or_else(|| self.builtin_args.get(a).map(|a| (None, a)));
                if let (Some((pg, pargs)), Some((ag, aargs))) = (pa, aa) {
                    // Option has one argument and Result two, so the count tells them apart.
                    let same_builtin = pg.is_none() && ag.is_none() && pargs.len() == aargs.len();
                    if (pg.is_some() && pg == ag) || same_builtin {
                        for (x, y) in pargs.iter().zip(aargs) {
                            self.unify(x, y, binds);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // ---- constraints ----

    /// Whether `t` meets `c`; a type parameter meets what it declares.
    pub(super) fn satisfies(&self, t: &Ty, c: Constraint) -> bool {
        match (t, c) {
            (Ty::Param(n), c) => self.param_constraints.get(n).is_some_and(|p| p.constraints.contains(&c)),
            (Ty::I64 | Ty::Str, _) => true,
            (Ty::F64(_), Constraint::Ordered | Constraint::Text) => true,
            (Ty::Bool, Constraint::Text) => true,
            (Ty::Error, _) => true,
            _ => false,
        }
    }

    /// E0701 for each type argument that does not meet its parameter's constraints.
    fn constraints_hold(&mut self, g: usize, args: &[Ty], at: Span) -> bool {
        let mut ok = true;
        let params = self.generics[g].params.clone();
        let name = self.generics[g].name.clone();
        for (p, a) in params.iter().zip(args) {
            for c in &p.constraints {
                if !self.satisfies(a, *c) {
                    ok = false;
                    let shown = self.show(a);
                    let (cn, meaning) = CONSTRAINTS.iter().find(|(_, k, _)| k == c).map(|(n, _, m)| (*n, *m)).unwrap();
                    let mut d = Diagnostic::new(
                        "E0701",
                        at,
                        format!("`{name}` needs `{}: {cn}`, but {} is {shown} here", p.name, p.name),
                    )
                    .note(format!("{cn} means {meaning}"), None);
                    if let Ty::Param(n) = a {
                        d = d.note(format!("add `{cn}` to `{n}` where it is declared"), None);
                    }
                    self.err(d);
                }
            }
        }
        ok
    }

    /// E0703 at an operation a type parameter cannot do without `c`, with the constraint as a fix.
    pub(super) fn needs_constraint(&mut self, t: &Ty, c: Constraint, span: Span, what: &str) {
        let Ty::Param(n) = t else { return };
        let cn = constraint_name(c);
        let mut d = Diagnostic::new("E0703", span, format!("{what} needs `{n}: {cn}`"));
        if let Some(p) = self.param_constraints.get(n) {
            let text = if p.has_constraints { format!(" + {cn}") } else { format!(": {cn}") };
            d = d.fix1(
                FixKind::ChangesMeaning,
                format!("declare `{n}: {cn}` (callers must then pass such a type)"),
                p.insert,
                text,
            );
        }
        self.err(d);
    }

    // ---- calls and literals ----

    /// A call to generic function `g`: infer the type arguments, check the constraints, then call
    /// the instance for them (or, inside a generic body, an abstract stand-in).
    pub(super) fn generic_call(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        callee: &ast::Ident,
        args: &[ast::Arg],
        g: usize,
    ) -> Expr {
        self.resolve_shape(g);
        let (params, ret, effects) = self.generics[g].sig.clone();
        let name = callee.name.as_str();
        let GenericKind::Fn(d) = self.generics[g].kind else { unreachable!() };
        let decl_span = d.name.span;
        let local = self.generics[g].module == self.module;
        self.note_use(callee.span, Some(self.generic_loc(g)));
        for eff in effects.iter() {
            self.note_effect(b, eff, callee.span, &format!("calling `{name}`"));
        }
        if d.noalloc.is_none() {
            // A fix would edit the generic's signature; only the message is given.
            self.note_alloc_call(b, callee.span, name, None);
        }
        let Some(true) = self.check_args(name, e.span, args, &params, local.then_some(decl_span)) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let binds = self.infer(b, args.iter().map(|a| &a.value), params.iter().map(|(_, t)| t));
        let tps = self.generics[g].params.clone();
        if tps.iter().any(|p| !binds.contains_key(&p.name)) {
            // Only when an argument had an error of its own: report those.
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        let targs: Vec<Ty> = tps.iter().map(|p| binds[&p.name].clone()).collect();
        if !self.constraints_hold(g, &targs, e.span) {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        let mut vals = Vec::new();
        for (a, (_, t)) in args.iter().zip(&params) {
            let want = self.subst(t, &binds);
            vals.push(self.expect(b, &a.value, &want));
        }
        let ret = self.subst(&ret, &binds);
        if vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        if self.abstract_body || targs.iter().any(|t| self.mentions_any_param(t)) {
            // Checked against constraints only; this IR is never run.
            return Expr { kind: ExprKind::Call(0, vals), ty: ret };
        }
        let id = self.instantiate_fn(g, targs, e.span);
        Expr { kind: ExprKind::Call(id, vals), ty: ret }
    }

    fn mentions_any_param(&self, t: &Ty) -> bool {
        self.param_constraints.keys().any(|n| self.mentions(t, n)) || matches!(t, Ty::Param(_))
    }

    /// Infers type parameters from argument values, evaluated quietly (their errors come later,
    /// when they are checked against the inferred types).
    fn infer<'x>(
        &mut self,
        b: &mut Body,
        values: impl Iterator<Item = &'x ast::Expr>,
        patterns: impl Iterator<Item = &'x Ty>,
    ) -> HashMap<String, Ty> {
        let mut binds = HashMap::new();
        self.quiet += 1;
        for (v, p) in values.zip(patterns) {
            if self.mentions_param_pattern(p) {
                let x = self.expr(b, v);
                self.unify(p, &x.ty, &mut binds);
            }
        }
        self.quiet -= 1;
        binds
    }

    /// Whether a callee's pattern type has any parameter in it.
    fn mentions_param_pattern(&self, t: &Ty) -> bool {
        match t {
            Ty::Param(_) => true,
            Ty::Array(e) => self.mentions_param_pattern(e),
            Ty::Map(k, v) => self.mentions_param_pattern(k) || self.mentions_param_pattern(v),
            Ty::Struct(id) => {
                self.struct_inst.get(id).is_some_and(|(_, a)| a.iter().any(|t| self.mentions_param_pattern(t)))
            }
            Ty::Enum(id) => match (self.enum_inst.get(id), self.builtin_args.get(id)) {
                (Some((_, a)), _) | (_, Some(a)) => a.iter().any(|t| self.mentions_param_pattern(t)),
                _ => false,
            },
            _ => false,
        }
    }

    /// The instance of generic type `g` a literal or constructor makes: the expected type's if it
    /// is one, otherwise the one its field values imply. None (after an error) if neither tells.
    pub(super) fn infer_instance(
        &mut self,
        b: &mut Body,
        g: usize,
        hint: Option<&Ty>,
        pairs: Vec<(&ast::Expr, Ty)>,
        span: Span,
        what: &str,
    ) -> Option<usize> {
        self.resolve_shape(g);
        match hint {
            Some(Ty::Struct(id)) if self.struct_inst.get(id).is_some_and(|(hg, _)| *hg == g) => return Some(*id),
            Some(Ty::Enum(id)) if self.enum_inst.get(id).is_some_and(|(hg, _)| *hg == g) => return Some(*id),
            _ => {}
        }
        let binds = self.infer(b, pairs.iter().map(|(v, _)| *v), pairs.iter().map(|(_, t)| t));
        let tps = self.generics[g].params.clone();
        let name = self.generics[g].name.clone();
        let missing: Vec<&str> = tps.iter().filter(|p| !binds.contains_key(&p.name)).map(|p| p.name.as_str()).collect();
        if !missing.is_empty() {
            let shape = tps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
            self.err(
                Diagnostic::new("E0404", span, format!("the type arguments of {what} cannot be inferred here")).note(
                    format!(
                        "state the type: let x: {name}[{shape}] = ...; ({} not shown by the values)",
                        missing.join(", ")
                    ),
                    None,
                ),
            );
            return None;
        }
        let targs: Vec<Ty> = tps.iter().map(|p| binds[&p.name].clone()).collect();
        if !self.constraints_hold(g, &targs, span) {
            return None;
        }
        match self.instantiate_type(g, targs) {
            Ty::Struct(id) | Ty::Enum(id) => Some(id),
            _ => None,
        }
    }

    /// The function instance of generic `g` for `args`; its body is checked at the end of the module.
    fn instantiate_fn(&mut self, g: usize, args: Vec<Ty>, site: Span) -> FuncId {
        let key = (g, format!("{args:?}"));
        if let Some(&id) = self.instances.get(&key) {
            return id;
        }
        let map = self.binding(g, &args);
        let (params, ret, effects) = self.generics[g].sig.clone();
        let params: Vec<(String, Ty)> = params.iter().map(|(n, t)| (n.clone(), self.subst(t, &map))).collect();
        let ret = self.subst(&ret, &map);
        let GenericKind::Fn(d) = self.generics[g].kind else { unreachable!() };
        let id = self.sigs.len();
        self.sigs.push(FuncSig {
            params,
            ret,
            effects,
            noalloc: d.noalloc.is_some(),
            noalloc_insert: super::noalloc_insert(d),
            span: d.name.span,
            module: self.generics[g].module,
        });
        self.instances.insert(key, id);
        let level = self.inst_level + 1;
        self.pending.push(Pending { id, generic: g, args, site: (self.module, site), level });
        id
    }

    // ---- bodies ----

    /// Checks the body of generic function `g` once, against its constraints; the IR is dropped.
    pub(super) fn check_generic_body(&mut self, g: usize) {
        self.resolve_shape(g);
        let GenericKind::Fn(d) = self.generics[g].kind else { return };
        let (params, ret, effects) = self.generics[g].sig.clone();
        let id = self.sigs.len();
        self.sigs.push(FuncSig {
            params,
            ret,
            effects,
            noalloc: d.noalloc.is_some(),
            noalloc_insert: super::noalloc_insert(d),
            span: d.name.span,
            module: self.module,
        });
        let was = std::mem::replace(&mut self.abstract_body, true);
        self.as_params(g, |c| c.func_at(d, id));
        self.abstract_body = was;
        self.sigs.truncate(id);
    }

    /// Checks the bodies of the instances asked for so far (which may ask for more), each in the
    /// module its generic lives in; returns them in FuncId order.
    pub(super) fn run_pending(&mut self, srcs: &[&'a SourceFile]) -> Vec<Func> {
        let mut out = Vec::new();
        while !self.pending.is_empty() {
            let p = self.pending.remove(0);
            let g = p.generic;
            let GenericKind::Fn(d) = self.generics[g].kind else { unreachable!() };
            let here = (self.module, self.src, self.save_scope());
            let home = self.generics[g].module;
            let scope = self.scopes[home].clone().expect("a generic's module is checked before it is used");
            self.load_scope(scope);
            self.module = home;
            self.src = srcs[home];
            let binding = self.binding(g, &p.args);
            let saved = std::mem::replace(&mut self.type_params, binding);
            let before = self.quiet_errors.len();
            self.quiet += 1;
            let outer = std::mem::replace(&mut self.inst_level, p.level);
            let mut f = if p.level > MAX_DEPTH { None } else { Some(self.func_at(d, p.id)) };
            self.inst_level = outer;
            self.quiet -= 1;
            let errors: Vec<Diagnostic> = self.quiet_errors.drain(before..).collect();
            self.type_params = saved;
            let module_name = self.module_names[home].clone();
            (self.module, self.src) = (here.0, here.1);
            self.load_scope(here.2);
            let shown: Vec<String> = p.args.iter().map(|a| self.show(a)).collect();
            let qualified =
                if module_name.is_empty() { d.name.name.clone() } else { format!("{module_name}.{}", d.name.name) };
            let label = format!("{qualified}[{}]", shown.join(", "));
            let f = match f.take() {
                Some(mut f) => {
                    let site = srcs[p.site.0].site(p.site.1);
                    for d in errors {
                        self.diags.push(d.note(format!("in `{label}`, used at {site}"), None));
                    }
                    f.name = label;
                    f.instance = true;
                    f
                }
                None => {
                    if !self.depth_reported {
                        self.depth_reported = true;
                        let mut diag = Diagnostic::new(
                            "E0704",
                            d.name.span,
                            format!("`{}` instantiates itself with ever larger types", d.name.name),
                        )
                        .note("a generic may not call itself with a type built from its own parameters", None);
                        diag.file = home;
                        self.diags.push(diag);
                    }
                    self.empty_func(d, home)
                }
            };
            out.push(f);
        }
        out
    }

    /// A stand-in for an instance that failed; the program has errors, so it never runs.
    fn empty_func(&self, d: &ast::FnDecl, module: usize) -> Func {
        Func {
            name: d.name.name.clone(),
            span: d.name.span,
            allows: Vec::new(),
            module,
            instance: true,
            params: Vec::new(),
            ret: Ty::Void,
            effects: Effects::default(),
            external: None,
            requires: Vec::new(),
            ensures: Vec::new(),
            result_local: None,
            locals: Vec::new(),
            body: Vec::new(),
        }
    }

    pub(super) fn save_scope(&mut self) -> Scope {
        Scope {
            func_index: self.func_index.clone(),
            struct_index: self.struct_index.clone(),
            enum_index: self.enum_index.clone(),
            units: self.units.clone(),
            type_spans: self.type_spans.clone(),
            unit_spans: self.unit_spans.clone(),
            generic_fns: self.generic_fns.clone(),
            generic_types: self.generic_types.clone(),
            imported: self.imported.clone(),
            failed_imports: self.failed_imports.clone(),
        }
    }

    fn load_scope(&mut self, s: Scope) {
        self.func_index = s.func_index;
        self.struct_index = s.struct_index;
        self.enum_index = s.enum_index;
        self.units = s.units;
        self.type_spans = s.type_spans;
        self.unit_spans = s.unit_spans;
        self.generic_fns = s.generic_fns;
        self.generic_types = s.generic_types;
        self.imported = s.imported;
        self.failed_imports = s.failed_imports;
    }
}
