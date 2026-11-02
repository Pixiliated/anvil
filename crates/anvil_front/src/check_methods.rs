//! Methods and interfaces. A method (`fn Circle.area(self)`) is a function declared on a type of
//! its own module and called on a value of it (`c.area()`); it is found by the value's type, so
//! it needs no import. An interface lists methods (`interface Area { fn area(self) -> f64; }`)
//! and is a constraint on type parameters (`T: Area`): a type meets it when it has every method
//! with the same signature, `Self` being the type. Interfaces are only constraints; generic
//! bodies call their methods, and each instance calls the type's own.

use super::generic::{Constraint, GenericKind};
use super::{Body, Checker, Loc};
use crate::ast;
use crate::diag::{Diagnostic, FixKind};
use crate::ir::*;
use crate::span::Span;
use std::collections::HashMap;

/// A method's signature in an interface, without `self`; `Self` is `Ty::Param("Self")`.
#[derive(Clone, Debug)]
pub(super) struct IfaceMethod {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    pub effects: Effects,
    pub noalloc: bool,
}

#[derive(Clone, Debug)]
pub(super) struct InterfaceDef {
    pub name: String,
    pub module: usize,
    pub span: Span,
    pub methods: Vec<IfaceMethod>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum MethodFn {
    Fn(FuncId),
    /// A generic function: a method of a generic type, or one with type parameters of its own.
    Generic(usize),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Method {
    pub func: MethodFn,
    pub module: usize,
    pub public: bool,
    /// The method's name in its declaration.
    pub span: Span,
}

/// The type a `Self` in an interface signature stands for while it is resolved.
const SELF: &str = "Self";

impl<'a> Checker<'a> {
    // ---- declarations ----

    /// The names of this module's interfaces, before anything that may use them as constraints.
    pub(super) fn declare_interfaces(&mut self, decls: &[&ast::InterfaceDecl]) {
        let mut seen = HashMap::new();
        for d in decls {
            if super::TYPE_NAMES.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in type", d.name.name)));
                continue;
            }
            if !self.check_new_name(&d.name, "type", &mut seen) {
                continue;
            }
            self.def(d.name.span, d.name.name.clone(), "interface", format!("interface {}", d.name.name));
            self.interface_index.insert(d.name.name.clone(), self.interfaces.len());
            self.interfaces.push(InterfaceDef {
                name: d.name.name.clone(),
                module: self.module,
                span: d.name.span,
                methods: Vec::new(),
            });
        }
    }

    /// The method signatures of this module's interfaces, once its types are declared.
    pub(super) fn resolve_interfaces(&mut self, decls: &[&ast::InterfaceDecl]) {
        for d in decls {
            let Some(&i) = self.interface_index.get(&d.name.name) else { continue };
            if self.interfaces[i].module != self.module || self.interfaces[i].span != d.name.span {
                continue;
            }
            let n = &d.name.name;
            if self.struct_index.contains_key(n)
                || self.enum_index.contains_key(n)
                || self.generic_types.contains_key(n)
            {
                self.err(Diagnostic::new(
                    "E0201",
                    d.name.span,
                    format!("`{n}` is the name of a type and of an interface"),
                ));
            }
            let saved = std::mem::take(&mut self.type_params);
            self.type_params.insert(SELF.into(), Ty::Param(SELF.into()));
            let mut methods: Vec<IfaceMethod> = Vec::new();
            for m in &d.methods {
                if methods.iter().any(|p| p.name == m.name.name) {
                    self.err(Diagnostic::new(
                        "E0201",
                        m.name.span,
                        format!("interface `{}` lists `{}` twice", d.name.name, m.name.name),
                    ));
                    continue;
                }
                let params: Vec<(String, Ty)> =
                    m.params[1..].iter().map(|p| (p.name.name.clone(), self.resolve_type(&p.ty))).collect();
                let ret = m.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Void);
                let effects = self.effects(&m.uses);
                let header = format!("interface {}: {}", d.name.name, self.text(m.span).trim_end_matches(';'));
                self.def(m.name.span, format!("{}.{}", d.name.name, m.name.name), "method", header);
                methods.push(IfaceMethod {
                    name: m.name.name.clone(),
                    params,
                    ret,
                    effects,
                    noalloc: m.noalloc.is_some(),
                });
            }
            self.type_params = saved;
            self.interfaces[i].methods = methods;
        }
    }

    /// The table key of the methods of `t`: a struct or enum, or (for an instance) its generic.
    pub(super) fn method_owner(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Struct(id) => Some(match self.struct_inst.get(id) {
                Some((g, _)) => format!("generic {g}"),
                None => format!("{t:?}"),
            }),
            Ty::Enum(id) if self.enum_owner[*id].is_some() => Some(match self.enum_inst.get(id) {
                Some((g, _)) => format!("generic {g}"),
                None => format!("{t:?}"),
            }),
            _ => None,
        }
    }

    /// Records method `d` of its receiver type, which must be declared in this module (E0415).
    pub(super) fn register_method(&mut self, d: &ast::FnDecl, func: MethodFn) {
        let r = d.receiver.as_ref().expect("a method has a receiver");
        let tname = &r.ty.name;
        if d.external.is_some() {
            self.err(
                Diagnostic::new("E0415", d.name.span, "an extern function cannot be a method")
                    .note("declare the extern, and a method that calls it", None),
            );
            return;
        }
        // The type, its parameter count, and whether this module declares it.
        let found: Option<(String, usize, bool)> = if let Some(&g) = self.generic_types.get(tname) {
            Some((format!("generic {g}"), self.generics[g].params.len(), self.generics[g].module == self.module))
        } else if let Some(&id) = self.struct_index.get(tname) {
            Some((format!("{:?}", Ty::Struct(id)), 0, self.struct_owner[id].0 == self.module))
        } else if let Some(&id) = self.enum_index.get(tname) {
            Some((format!("{:?}", Ty::Enum(id)), 0, matches!(self.enum_owner[id], Some((m, _)) if m == self.module)))
        } else {
            None
        };
        let (owner, arity) = match found {
            Some((owner, arity, true)) => (owner, arity),
            Some((_, _, false)) => {
                self.err(
                    Diagnostic::new("E0415", r.ty.span, format!("`{tname}` is declared in another module"))
                        .note("methods are declared in the module of their type; write a function here instead", None),
                );
                return;
            }
            None => {
                let msg = if super::TYPE_NAMES.contains(&tname.as_str()) {
                    format!("`{tname}` is a built-in type, which has no methods of yours")
                } else {
                    format!("`{tname}` is not a struct or enum of this module")
                };
                self.err(
                    Diagnostic::new("E0415", r.ty.span, msg)
                        .note("methods are declared on structs and enums, in the module that declares them", None),
                );
                return;
            }
        };
        if r.type_params.len() != arity {
            let msg = if arity == 0 {
                format!("`{tname}` has no type parameters: write fn {tname}.{}(self)", d.name.name)
            } else {
                format!("`{tname}` has {arity} type parameter(s); name them all: fn {tname}[T].{}(self)", d.name.name)
            };
            self.err(Diagnostic::new("E0307", r.ty.span, msg));
            return;
        }
        let name = &d.name.name;
        let clash = if let Some(&id) = self.struct_index.get(tname) {
            self.structs[id].fields.iter().any(|(f, _)| f == name)
        } else if let Some(&g) = self.generic_types.get(tname) {
            match self.generics[g].kind {
                GenericKind::Struct(s) => s.fields.iter().any(|(f, _)| f.name == *name),
                GenericKind::Enum(e) => e.variants.iter().any(|v| v.name.name == *name),
                GenericKind::Fn(_) => false,
            }
        } else if let Some(&id) = self.enum_index.get(tname) {
            self.enums[id].variants.iter().any(|v| v.name == *name)
        } else {
            false
        };
        let table = self.methods.entry(owner).or_default();
        if let Some(prev) = table.get(name) {
            let prev = prev.span;
            self.err(
                Diagnostic::new("E0201", d.name.span, format!("`{tname}.{name}` is declared twice"))
                    .note("first declared here", Some(prev)),
            );
            return;
        }
        if clash {
            self.err(Diagnostic::new(
                "E0201",
                d.name.span,
                format!("`{tname}` already has a field or variant named `{name}`"),
            ));
            return;
        }
        table.insert(name.clone(), Method { func, module: self.module, public: d.public, span: d.name.span });
    }

    // ---- calls ----

    /// Whether `.name(...)` on a value of `t` is a method of the program's (not a built-in one).
    pub(super) fn has_methods(&self, t: &Ty) -> bool {
        match t {
            Ty::Struct(_) | Ty::Param(_) => true,
            Ty::Enum(id) => self.enum_owner[*id].is_some(),
            _ => false,
        }
    }

    /// `receiver.method(args)` where the receiver's type is `rt` (a struct, an enum or a type
    /// parameter); `pre` is the receiver already checked, when it was.
    pub(super) fn user_method(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        receiver: &ast::Expr,
        (rt, pre): (Ty, Option<Expr>),
        method: &ast::Ident,
        args: &[ast::Arg],
    ) -> Expr {
        let shown = self.show(&rt);
        if let Ty::Param(n) = &rt {
            return self.interface_call(b, e, (receiver, pre), n.clone(), method, args);
        }
        let owner = self.method_owner(&rt).expect("structs and enums have a method table");
        let found = self.methods.get(&owner).and_then(|ms| ms.get(&method.name)).copied();
        let Some(m) = found else {
            let names: Vec<String> =
                self.methods.get(&owner).map(|ms| ms.keys().cloned().collect()).unwrap_or_default();
            let mut sorted = names.clone();
            sorted.sort();
            let list = if sorted.is_empty() {
                "it has none".to_string()
            } else {
                format!("its methods: {}", sorted.join(", "))
            };
            let mut d = Diagnostic::new("E0406", method.span, format!("{shown} has no method `{}`", method.name))
                .note(list, None);
            if let Ty::Struct(id) = rt {
                if self.structs[id].fields.iter().any(|(f, _)| *f == method.name) {
                    d = d.note(format!("`{}` is a field: read it without ( )", method.name), None);
                }
            }
            if let Some(s) = super::closest(&method.name, names.iter()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), method.span, s);
            }
            self.err(d);
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let base = shown.split('[').next().unwrap_or(&shown).to_string();
        let full = format!("{base}.{}", method.name);
        // An instance's body calls what its interfaces let it, whoever declared the method.
        if !m.public && m.module != self.module && self.inst_level == 0 {
            let module = self.module_names[m.module].clone();
            self.err(
                Diagnostic::new("E0603", method.span, format!("`{full}` is private to module `{module}`"))
                    .note(format!("mark it pub in module `{module}` to call it here"), None),
            );
        }
        self.note_use(method.span, Some(Loc { file: m.module, span: m.span }));
        match m.func {
            MethodFn::Generic(g) => self.generic_call(b, e, method, args, g, Some(receiver)),
            MethodFn::Fn(fid) => self.method_call(b, e, (receiver, pre), &full, method, args, fid),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn method_call(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        (receiver, pre): (&ast::Expr, Option<Expr>),
        full: &str,
        method: &ast::Ident,
        args: &[ast::Arg],
        fid: FuncId,
    ) -> Expr {
        let s = &self.sigs[fid];
        let (params, ret, effects, noalloc) = (s.params.clone(), s.ret.clone(), s.effects, s.noalloc);
        for eff in effects.iter() {
            self.note_effect(b, eff, method.span, &format!("calling `{full}`"));
        }
        if !noalloc {
            self.note_alloc_call(b, method.span, full, Some(fid));
        }
        let local = self.sigs[fid].module == self.module;
        let sig_span = local.then_some(self.sigs[fid].span);
        let this = match pre {
            Some(x) => x,
            None => self.expect(b, receiver, &params[0].1),
        };
        let Some(ok) = self.check_args(full, e.span, args, &params[1..], sig_span) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let mut vals = vec![this];
        for (a, (_, t)) in args.iter().zip(&params[1..]) {
            vals.push(self.expect(b, &a.value, t));
        }
        if !ok || vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::Call(fid, vals), ty: ret }
    }

    /// `x.m(args)` on a value of type parameter `n` in a generic body: a method of one of its
    /// interfaces. The IR is never run (instances call the type's method).
    fn interface_call(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        (receiver, pre): (&ast::Expr, Option<Expr>),
        n: String,
        method: &ast::Ident,
        args: &[ast::Arg],
    ) -> Expr {
        let ifaces: Vec<usize> = self
            .param_constraints
            .get(&n)
            .map(|p| {
                p.constraints
                    .iter()
                    .filter_map(|c| if let Constraint::Interface(i) = c { Some(*i) } else { None })
                    .collect()
            })
            .unwrap_or_default();
        let found = ifaces
            .iter()
            .find_map(|&i| self.interfaces[i].methods.iter().find(|m| m.name == method.name).map(|m| (i, m.clone())));
        let Some((i, im)) = found else {
            let names: Vec<String> =
                ifaces.iter().flat_map(|&i| self.interfaces[i].methods.iter().map(|m| m.name.clone())).collect();
            let note = if ifaces.is_empty() {
                format!("give `{n}` an interface that has it: [{n}: Name]")
            } else if names.is_empty() {
                "its interfaces have no methods".to_string()
            } else {
                format!("its interfaces have: {}", names.join(", "))
            };
            self.err(
                Diagnostic::new("E0406", method.span, format!("`{n}` has no method `{}`", method.name))
                    .note(note, None),
            );
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let iname = self.interfaces[i].name.clone();
        let full = format!("{n}.{}", im.name);
        for eff in im.effects.iter() {
            self.note_effect(b, eff, method.span, &format!("calling `{full}` ({iname})"));
        }
        if !im.noalloc {
            self.note_alloc_call(b, method.span, &full, None);
        }
        let map: HashMap<String, Ty> = [(SELF.to_string(), Ty::Param(n.clone()))].into_iter().collect();
        let params: Vec<(String, Ty)> = im.params.iter().map(|(p, t)| (p.clone(), self.subst(t, &map))).collect();
        let ret = self.subst(&im.ret, &map);
        let this = match pre {
            Some(x) => x,
            None => self.expect(b, receiver, &Ty::Param(n.clone())),
        };
        let Some(ok) = self.check_args(&full, e.span, args, &params, None) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let mut vals = vec![this];
        for (a, (_, t)) in args.iter().zip(&params) {
            vals.push(self.expect(b, &a.value, t));
        }
        if !ok || vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::Call(0, vals), ty: ret }
    }

    // ---- interfaces as constraints ----

    /// Whether `t` has every method of interface `i`, each with its signature (`Self` being `t`);
    /// Err says what is missing or different.
    pub(super) fn implements(&mut self, t: &Ty, i: usize) -> Result<(), String> {
        let shown = self.show(t);
        let iname = self.interfaces[i].name.clone();
        let Some(owner) = self.method_owner(t) else {
            return Err(format!("{shown} is not a struct or enum, so it has no methods"));
        };
        let base = shown.split('[').next().unwrap_or(&shown).to_string();
        let map: HashMap<String, Ty> = [(SELF.to_string(), t.clone())].into_iter().collect();
        for im in self.interfaces[i].methods.clone() {
            let full = format!("{base}.{}", im.name);
            let Some(m) = self.methods.get(&owner).and_then(|ms| ms.get(&im.name)).copied() else {
                return Err(format!("{shown} has no method `{}`, which {iname} needs", im.name));
            };
            let (params, ret, effects, noalloc) = match m.func {
                MethodFn::Fn(fid) => {
                    let s = &self.sigs[fid];
                    (s.params[1..].to_vec(), s.ret.clone(), s.effects, s.noalloc)
                }
                MethodFn::Generic(g) => {
                    self.resolve_shape(g);
                    let (ps, r, eff) = self.generics[g].sig.clone();
                    let noalloc = matches!(self.generics[g].kind, GenericKind::Fn(d) if d.noalloc.is_some());
                    let mut binds = HashMap::new();
                    self.unify(&ps[0].1, t, &mut binds);
                    if self.generics[g].params.iter().any(|p| !binds.contains_key(&p.name)) {
                        return Err(format!("`{full}` has type parameters of its own; an interface method has none"));
                    }
                    let ps: Vec<(String, Ty)> =
                        ps[1..].iter().map(|(n, t)| (n.clone(), self.subst(t, &binds))).collect();
                    (ps, self.subst(&r, &binds), eff, noalloc)
                }
            };
            let want: Vec<Ty> = im.params.iter().map(|(_, t)| self.subst(t, &map)).collect();
            let want_ret = self.subst(&im.ret, &map);
            let have: Vec<Ty> = params.iter().map(|(_, t)| t.clone()).collect();
            let sig = |c: &Self, ps: &[Ty], r: &Ty, eff: Effects, na: bool| {
                let shown_ty =
                    Ty::Fn(Box::new(FnTy { params: ps.to_vec(), ret: r.clone(), effects: eff, noalloc: na }));
                c.show(&shown_ty)
            };
            if have != want || ret != want_ret || !effects.within(im.effects) || (im.noalloc && !noalloc) {
                return Err(format!(
                    "`{full}` is {} (after self), and {iname} needs {}",
                    sig(self, &have, &ret, effects, noalloc),
                    sig(self, &want, &want_ret, im.effects, im.noalloc)
                ));
            }
        }
        Ok(())
    }

    /// Where interface `i` is declared.
    pub(super) fn interface_loc(&self, i: usize) -> Loc {
        Loc { file: self.interfaces[i].module, span: self.interfaces[i].span }
    }
}
