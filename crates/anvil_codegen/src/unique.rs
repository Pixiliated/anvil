//! When an array needs no check that it has one owner.
//!
//! A write to an array first makes sure nothing else holds it. A local that passed that check, or
//! was given a new array, stays the only owner until something shares it, so later writes need no
//! check: nothing can get at an array without going through its one reference. Skipping the check
//! also leaves no call on the way through a loop, so the loop's values stay in registers.
//!
//! `changes` says which locals a statement might share or give a new value, and which arrays it
//! might shorten; `Gen::stmt` forgets what it knew about them around that statement.
//!
//! The same goes for indices. A local used as an index that has passed a bounds check stays good
//! until the local or the array changes, so a swap checks each index once, and a counter up to an
//! array's length never needs one.

use anvil_front::ir::*;
use std::collections::{HashMap, HashSet};

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

/// A scalar of a local that holds an array or bytes.
pub(crate) type Key = (LocalId, usize);

/// What is known at one point of a function.
#[derive(Clone, Default)]
pub(crate) struct Known {
    /// Arrays with one owner.
    pub unique: HashSet<Key>,
    /// Which locals are good indices of which arrays.
    pub in_bounds: HashSet<(Key, LocalId)>,
    /// Locals that hold an array's length, or less.
    pub lengths: HashMap<LocalId, Key>,
    /// Counters of `for` loops from a number not below 0, which nothing else changes.
    pub counters: HashSet<LocalId>,
}

impl Known {
    pub fn forget(&mut self, changes: &Changes) {
        let gone = |l: &LocalId| changes.touched.contains(l);
        let cut = |l: &LocalId| gone(l) || changes.shrunk.contains(l);
        self.unique.retain(|(l, _)| !gone(l));
        self.in_bounds.retain(|((a, _), i)| !cut(a) && !gone(i));
        self.lengths.retain(|n, (a, _)| !gone(n) && !cut(a));
        self.counters.retain(|i| !gone(i));
    }

    /// Keeps what also holds in `other`: where two paths join.
    pub fn meet(&mut self, other: &Known) {
        self.unique.retain(|k| other.unique.contains(k));
        self.in_bounds.retain(|k| other.in_bounds.contains(k));
        self.lengths.retain(|n, a| other.lengths.get(n) == Some(a));
        self.counters.retain(|i| other.counters.contains(i));
    }
}

/// What a statement might do to locals.
#[derive(Default)]
pub(crate) struct Changes {
    /// Shared with something else, or given a new value.
    pub touched: HashSet<LocalId>,
    /// Holding an array that may get shorter.
    pub shrunk: HashSet<LocalId>,
}

impl Changes {
    pub fn merge(&mut self, other: Changes) {
        self.touched.extend(other.touched);
        self.shrunk.extend(other.shrunk);
    }
}

/// Which local's scalar an expression reads through fields alone, if it does.
pub(crate) fn key_of(program: &Program, e: &Expr) -> Option<Key> {
    match &e.kind {
        ExprKind::Local(l) => Some((*l, 0)),
        ExprKind::Field(base, i) => {
            let (l, start) = key_of(program, base)?;
            let Ty::Struct(sid) = base.ty else { return None };
            Some((l, start + crate::field_range(program, sid, *i).0))
        }
        _ => None,
    }
}

/// The number local an index or an operand is, if it is one (`Move` being its last read).
pub(crate) fn local_of(e: &Expr) -> Option<LocalId> {
    match e.kind {
        ExprKind::Local(l) | ExprKind::Move(l) => Some(l),
        _ => None,
    }
}

struct Walk<'a> {
    program: &'a Program,
    out: &'a mut HashSet<LocalId>,
    shrunk: Option<&'a mut HashSet<LocalId>>,
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
            // Takes an element, after the check a write makes: the array stays where it is.
            ExprKind::Take { path, .. } if path.iter().any(|step| matches!(step, Step::Index { .. })) => {
                for c in children(e) {
                    self.expr(c);
                }
            }
            // Changes the array the local holds in place, after a check of its own.
            ExprKind::Push { .. } | ExprKind::Pop { .. } | ExprKind::MapSet { .. } | ExprKind::MapRemove { .. } => {
                if let (ExprKind::Pop { local, path, .. } | ExprKind::MapRemove { local, path, .. }, Some(shrunk)) = (&e.kind, self.shrunk.as_deref_mut()) {
                    if !path.iter().any(|step| matches!(step, Step::Index { .. })) {
                        shrunk.insert(*local);
                    }
                }
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

/// Whether `s` might leave the loop's round it is part of, other than by a panic.
fn might_leave(s: &Stmt) -> bool {
    fn expr(e: &Expr) -> bool {
        matches!(e.kind, ExprKind::Try { .. }) || children(e).into_iter().any(expr)
    }
    fn path(p: &[Step]) -> bool {
        p.iter().any(|step| matches!(step, Step::Index { index, .. } if expr(index)))
    }
    fn block(b: &[Stmt]) -> bool {
        b.iter().any(might_leave)
    }
    match s {
        Stmt::Return(_) | Stmt::Break | Stmt::Continue => true,
        Stmt::Let(_, e) | Stmt::Expr(e) => expr(e),
        Stmt::Assign(_, p, e, _) => path(p) || expr(e),
        Stmt::If(c, t, e) => expr(c) || block(t) || block(e),
        Stmt::While(c, b) => expr(c) || block(b),
        Stmt::For(_, start, end, b) => expr(start) || expr(end) || block(b),
        Stmt::ForEach { iter, body, .. } => expr(iter) || block(body),
        Stmt::Match { scrutinee, arms, .. } => expr(scrutinee) || arms.iter().any(|a| block(&a.body)),
        Stmt::Drop(_) | Stmt::Reached(_) | Stmt::Line(_) => false,
    }
}

/// The scalar of local `l` that `path` writes into, with the array's type, if it goes through an
/// array that fields alone reach.
fn written_array(program: &Program, types: &[Ty], l: LocalId, path: &[Step]) -> Option<(Key, Ty)> {
    let (mut ty, mut start) = (&types[l], 0);
    for step in path {
        match step {
            Step::Field(i) => {
                let Ty::Struct(sid) = ty else { unreachable!("field of {ty:?}") };
                start += crate::field_range(program, *sid, *i).0;
                ty = &program.structs[*sid].fields[*i].1;
            }
            Step::Index { .. } => break,
        }
    }
    matches!(ty, Ty::Array(_)).then(|| ((l, start), ty.clone()))
}

/// The arrays a round of a loop is sure to write, once it starts. The check for a second owner can
/// come before the loop, where any copy it makes is one the first round would have made anyway.
pub(crate) fn surely_written(program: &Program, types: &[Ty], body: &[Stmt]) -> Vec<(Key, Ty)> {
    let mut out = Vec::new();
    for s in body {
        if might_leave(s) {
            break;
        }
        let place = match s {
            Stmt::Assign(l, path, _, _) if path.iter().any(|step| matches!(step, Step::Index { .. })) => Some((*l, path)),
            Stmt::Expr(Expr { kind: ExprKind::Push { local, path, .. }, .. }) => Some((*local, path)),
            _ => None,
        };
        if let Some((key, ty)) = place.and_then(|(l, path)| written_array(program, types, l, path)) {
            if !out.iter().any(|(k, _)| *k == key) {
                out.push((key, ty));
            }
        }
    }
    out
}

/// Whether evaluating `e` again straight away gives the same value and does nothing else.
pub(crate) fn repeats(e: &Expr) -> bool {
    let plain = match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Local(_) | ExprKind::Field(..) => true,
        ExprKind::And(..) | ExprKind::Or(..) | ExprKind::Not(_) | ExprKind::BitNot(_) | ExprKind::FNeg(_) | ExprKind::FArith(..) => true,
        ExprKind::IArith { .. } | ExprKind::INeg { .. } | ExprKind::Bits { .. } => true,
        ExprKind::Cmp(_, l, _) => matches!(l.ty, Ty::I64 | Ty::F64(_) | Ty::Bool),
        ExprKind::Index { plain, .. } => *plain,
        ExprKind::Builtin(Builtin::Len, _) => true,
        _ => false,
    };
    plain && children(e).into_iter().all(repeats)
}

/// Whether a block always ends by leaving it: what follows is not reached through its end.
pub(crate) fn leaves(stmts: &[Stmt]) -> bool {
    let last = stmts.iter().rev().find(|s| !matches!(s, Stmt::Line(_) | Stmt::Reached(_)));
    matches!(last, Some(Stmt::Return(_) | Stmt::Break | Stmt::Continue))
}

/// What `s` might do to locals.
pub(crate) fn changes(program: &Program, s: &Stmt) -> Changes {
    let mut c = Changes::default();
    Walk { program, out: &mut c.touched, shrunk: Some(&mut c.shrunk) }.stmt(s);
    c
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
            Stmt::Let(_, e) => Walk { program, out: &mut *out, shrunk: None }.expr(e),
            Stmt::Assign(_, path, e, _) if path.is_empty() => Walk { program, out: &mut *out, shrunk: None }.expr(e),
            Stmt::Return(Some(e)) if matches!(e.kind, ExprKind::Local(_) | ExprKind::Move(_)) => {}
            Stmt::If(c, t, e) => {
                Walk { program, out: &mut *out, shrunk: None }.expr(c);
                other_uses(program, t, out);
                other_uses(program, e, out);
            }
            Stmt::While(c, b) => {
                Walk { program, out: &mut *out, shrunk: None }.expr(c);
                other_uses(program, b, out);
            }
            Stmt::For(l, start, end, b) => {
                out.insert(*l);
                Walk { program, out: &mut *out, shrunk: None }.expr(start);
                Walk { program, out: &mut *out, shrunk: None }.expr(end);
                other_uses(program, b, out);
            }
            Stmt::ForEach { var, held, iter, body } => {
                out.insert(*var);
                out.insert(*held);
                Walk { program, out: &mut *out, shrunk: None }.expr(iter);
                other_uses(program, body, out);
            }
            Stmt::Match { held, scrutinee, arms } => {
                out.insert(*held);
                Walk { program, out: &mut *out, shrunk: None }.expr(scrutinee);
                for arm in arms {
                    for (_, local) in &arm.bindings {
                        out.insert(*local);
                    }
                    other_uses(program, &arm.body, out);
                }
            }
            s => Walk { program, out: &mut *out, shrunk: None }.stmt(s),
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
