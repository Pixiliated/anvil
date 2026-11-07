//! When an array needs no check that it has one owner.
//!
//! A write to an array first makes sure nothing else holds it. A local that passed that check, or
//! was given a new array, stays the only owner until something shares it, so later writes need no
//! check: nothing can get at an array without going through its one reference. Skipping the check
//! also leaves no call on the way through a loop, so the loop's values stay in registers.
//!
//! `touched` says which locals a statement might share or give a new value; `Gen::stmt` forgets
//! what it knew about them around that statement.

use anvil_front::ir::*;
use std::collections::HashSet;

/// The local an expression reads through fields alone, if it does.
fn root(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        ExprKind::Local(l) => Some(*l),
        ExprKind::Field(base, _) => root(base),
        _ => None,
    }
}

fn holds_heap(program: &Program, ty: &Ty) -> bool {
    crate::layout(program, ty).iter().any(|s| s.heap)
}

struct Walk<'a> {
    program: &'a Program,
    out: &'a mut HashSet<LocalId>,
}

impl Walk<'_> {
    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn path(&mut self, path: &[Step]) {
        for step in path {
            if let Step::Index { index, .. } = step {
                self.expr(index);
            }
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l, e) => {
                self.out.insert(*l);
                self.expr(e);
            }
            Stmt::Assign(l, path, e, _) => {
                // A write through an index changes an element, not the array the local holds.
                if !path.iter().any(|step| matches!(step, Step::Index { .. })) {
                    self.out.insert(*l);
                }
                self.path(path);
                self.expr(e);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::If(c, t, e) => {
                self.expr(c);
                self.stmts(t);
                self.stmts(e);
            }
            Stmt::While(c, body) => {
                self.expr(c);
                self.stmts(body);
            }
            Stmt::For(l, start, end, body) => {
                self.out.insert(*l);
                self.expr(start);
                self.expr(end);
                self.stmts(body);
            }
            Stmt::ForEach { var, held, iter, body } => {
                self.out.insert(*var);
                self.out.insert(*held);
                self.expr(iter);
                self.stmts(body);
            }
            Stmt::Match { held, scrutinee, arms } => {
                self.out.insert(*held);
                self.expr(scrutinee);
                for arm in arms {
                    for (_, local) in &arm.bindings {
                        self.out.insert(*local);
                    }
                    self.stmts(&arm.body);
                }
            }
            Stmt::Return(Some(e)) => self.expr(e),
            // Nothing reads a local after it is dropped until it is written, which counts by itself.
            Stmt::Drop(_) | Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Reached(_) | Stmt::Line(_) => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Local(_) | ExprKind::Field(..) if root(e).is_some() => {
                // Reading a number from a struct leaves its arrays as they were.
                if holds_heap(self.program, &e.ty) {
                    self.out.insert(root(e).unwrap());
                }
            }
            // Read through a borrowed reference (see `plain`), which it does not keep.
            ExprKind::Index { base, index, plain: true, .. } if root(base).is_some() => self.expr(index),
            ExprKind::Builtin(Builtin::Len, args) if root(&args[0]).is_some() => {}
            ExprKind::Builtin(Builtin::MapHas | Builtin::MapGet, args) if root(&args[0]).is_some() && !mutates(&args[1]) => {
                self.expr(&args[1])
            }
            ExprKind::Builtin(Builtin::MapKeys | Builtin::MapValues, args) if root(&args[0]).is_some() => {}
            // The last read: nothing reads the local again until it is written, as after a drop.
            ExprKind::Move(_) => {}
            // Changes the array the local holds in place, after a check of its own.
            ExprKind::Push { .. } | ExprKind::Pop { .. } | ExprKind::MapSet { .. } | ExprKind::MapRemove { .. } => {
                for c in children(e) {
                    self.expr(c);
                }
            }
            _ => {
                if let Some(l) = named_local(e) {
                    self.out.insert(l);
                }
                for c in children(e) {
                    self.expr(c);
                }
            }
        }
    }
}

/// The locals that `s` might share with something else or give a new value.
pub(crate) fn touched(program: &Program, s: &Stmt, out: &mut HashSet<LocalId>) {
    Walk { program, out }.stmt(s);
}

/// Whether `e` always gives an array or map with one reference, the one it hands over.
/// `fresh` says which functions return such a value.
pub(crate) fn is_fresh(e: &Expr, fresh: &[bool]) -> bool {
    if !matches!(e.ty, Ty::Array(_) | Ty::Map(..)) {
        return false;
    }
    match &e.kind {
        ExprKind::ArrayLit(_) | ExprKind::MapLit { .. } | ExprKind::Slice { .. } => true,
        ExprKind::Call(f, _, _) => fresh[*f],
        ExprKind::Cond { then, otherwise, .. } => is_fresh(then, fresh) && is_fresh(otherwise, fresh),
        ExprKind::Builtin(b, _) => matches!(
            b,
            Builtin::MapKeys
                | Builtin::MapValues
                | Builtin::StrSplit { .. }
                | Builtin::StrChars
                | Builtin::ArrSorted { .. }
                | Builtin::ArrReversed
                | Builtin::BytesToArray
        ),
        _ => false,
    }
}

/// Every `return` value in `stmts`, and every value given to a whole local, as (local, value).
fn gather<'a>(stmts: &'a [Stmt], returns: &mut Vec<&'a Expr>, defs: &mut Vec<(LocalId, &'a Expr)>) {
    for s in stmts {
        match s {
            Stmt::Let(l, e) => defs.push((*l, e)),
            Stmt::Assign(l, path, e, _) if path.is_empty() => defs.push((*l, e)),
            Stmt::Return(Some(e)) => returns.push(e),
            Stmt::If(_, t, e) => {
                gather(t, returns, defs);
                gather(e, returns, defs);
            }
            Stmt::While(_, b) | Stmt::For(_, _, _, b) | Stmt::ForEach { body: b, .. } => gather(b, returns, defs),
            Stmt::Match { arms, .. } => {
                for arm in arms {
                    gather(&arm.body, returns, defs);
                }
            }
            _ => {}
        }
    }
}

/// The locals named in `stmts` other than by a definition or a `return` of the local itself: what
/// those two do is judged by `returns_fresh`.
fn other_uses(program: &Program, stmts: &[Stmt], out: &mut HashSet<LocalId>) {
    for s in stmts {
        match s {
            Stmt::Let(_, e) => Walk { program, out: &mut *out }.expr(e),
            Stmt::Assign(_, path, e, _) if path.is_empty() => Walk { program, out: &mut *out }.expr(e),
            Stmt::Return(Some(e)) if matches!(e.kind, ExprKind::Local(_) | ExprKind::Move(_)) => {}
            Stmt::If(c, t, e) => {
                Walk { program, out: &mut *out }.expr(c);
                other_uses(program, t, out);
                other_uses(program, e, out);
            }
            Stmt::While(c, b) => {
                Walk { program, out: &mut *out }.expr(c);
                other_uses(program, b, out);
            }
            Stmt::For(l, start, end, b) => {
                out.insert(*l);
                Walk { program, out: &mut *out }.expr(start);
                Walk { program, out: &mut *out }.expr(end);
                other_uses(program, b, out);
            }
            Stmt::ForEach { var, held, iter, body } => {
                out.insert(*var);
                out.insert(*held);
                Walk { program, out: &mut *out }.expr(iter);
                other_uses(program, body, out);
            }
            Stmt::Match { held, scrutinee, arms } => {
                out.insert(*held);
                Walk { program, out: &mut *out }.expr(scrutinee);
                for arm in arms {
                    for (_, local) in &arm.bindings {
                        out.insert(*local);
                    }
                    other_uses(program, &arm.body, out);
                }
            }
            s => Walk { program, out: &mut *out }.stmt(s),
        }
    }
}

fn returns_fresh(program: &Program, f: &Func, fresh: &[bool]) -> bool {
    let (mut returns, mut defs) = (Vec::new(), Vec::new());
    gather(&f.body, &mut returns, &mut defs);
    let mut others = None;
    returns.iter().all(|e| {
        let l = match e.kind {
            ExprKind::Local(l) | ExprKind::Move(l) => l,
            _ => return is_fresh(e, fresh),
        };
        // A local that only ever held new arrays and that nothing else shared.
        let others = others.get_or_insert_with(|| {
            let mut out = HashSet::new();
            other_uses(program, &f.body, &mut out);
            out
        });
        !f.params.contains(&l)
            && !others.contains(&l)
            && defs.iter().filter(|(d, _)| *d == l).all(|(_, v)| is_fresh(v, fresh))
    })
}

/// For each function, whether it always returns an array or map with one reference. Starts by
/// assuming so and takes it back until nothing changes, so calls among them can count.
pub(crate) fn fresh_functions(program: &Program) -> Vec<bool> {
    let mut fresh: Vec<bool> = program
        .funcs
        .iter()
        .map(|f| {
            matches!(f.ret, Ty::Array(_) | Ty::Map(..))
                && f.external.is_none()
                && f.ensures.is_empty()
                && f.captures.is_none()
        })
        .collect();
    loop {
        let mut changed = false;
        for (i, f) in program.funcs.iter().enumerate() {
            if fresh[i] && !returns_fresh(program, f, &fresh) {
                fresh[i] = false;
                changed = true;
            }
        }
        if !changed {
            return fresh;
        }
    }
}
