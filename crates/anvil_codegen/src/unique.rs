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
use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// What locals are known to be here, beyond what any assignment to them gives (see `range`).
    pub ranges: HashMap<LocalId, crate::range::Range>,
}

impl Known {
    pub fn forget(&mut self, changes: &Changes) {
        let gone = |l: &LocalId| changes.touched.contains(l);
        let cut = |l: &LocalId| gone(l) || changes.shrunk.contains(l);
        self.unique.retain(|(l, _)| !gone(l));
        self.in_bounds.retain(|((a, _), i)| !cut(a) && !gone(i));
        self.lengths.retain(|n, (a, _)| !gone(n) && !cut(a));
        self.counters.retain(|i| !gone(i));
        self.ranges.retain(|i, _| !gone(i));
    }

    /// Keeps what also holds in `other`: where two paths join.
    pub fn meet(&mut self, other: &Known) {
        self.unique.retain(|k| other.unique.contains(k));
        self.in_bounds.retain(|k| other.in_bounds.contains(k));
        self.lengths.retain(|n, a| other.lengths.get(n) == Some(a));
        self.counters.retain(|i| other.counters.contains(i));
        self.ranges.retain(|i, _| other.ranges.contains_key(i));
        for (i, r) in self.ranges.iter_mut() {
            *r = crate::range::either(*r, other.ranges[i]);
        }
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

/// An array access in a loop's body that a test before the loop can clear.
pub(crate) struct Access<'e> {
    pub key: Key,
    /// The element's type; None for bytes, which live in their object.
    pub elem: Option<Ty>,
    pub index: &'e Expr,
    pub write: bool,
}

/// Locals a loop's body empties, or whose array it grows or shortens, however that shows.
fn not_held(body: &[Stmt]) -> HashSet<LocalId> {
    fn expr(e: &Expr, out: &mut HashSet<LocalId>) {
        match &e.kind {
            ExprKind::Move(l)
            | ExprKind::Take { local: l, .. }
            | ExprKind::Push { local: l, .. }
            | ExprKind::Pop { local: l, .. }
            | ExprKind::MapSet { local: l, .. }
            | ExprKind::MapRemove { local: l, .. } => {
                out.insert(*l);
            }
            _ => {}
        }
        for c in children(e) {
            expr(c, out);
        }
    }
    fn block(b: &[Stmt], out: &mut HashSet<LocalId>) {
        for s in b {
            match s {
                Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, out),
                Stmt::Assign(_, path, e, _) => {
                    for step in path {
                        if let Step::Index { index, .. } = step {
                            expr(index, out);
                        }
                    }
                    expr(e, out);
                }
                Stmt::If(c, t, f) => {
                    expr(c, out);
                    block(t, out);
                    block(f, out);
                }
                Stmt::Match { scrutinee, arms, .. } => {
                    expr(scrutinee, out);
                    for arm in arms {
                        block(&arm.body, out);
                    }
                }
                Stmt::Drop(l) => {
                    out.insert(*l);
                }
                Stmt::While(..) | Stmt::For(..) | Stmt::ForEach { .. } => unreachable!("only innermost loops are asked about"),
                Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Reached(_) | Stmt::Line(_) => {}
            }
        }
    }
    let mut out = HashSet::new();
    block(body, &mut out);
    out
}

/// Arrays and bytes a function indexes in a loop whose length and storage it can keep in locals: those
/// in locals that nothing treats as a map. A value is never changed in place while anything else holds
/// it, calls included, so what such a local holds changes its length or storage only where the local
/// is stored to, copied on a write, pushed to or popped from, and each of those reads them again. The value is whether there is a storage pointer to keep,
/// which bytes, living in their object, do not have.
pub(crate) fn held_arrays(program: &Program, types: &[Ty], f: &Func) -> BTreeMap<Key, bool> {
    #[derive(Default)]
    struct Found {
        wanted: BTreeMap<Key, bool>,
        grown: HashSet<LocalId>,
    }
    fn expr(program: &Program, e: &Expr, looping: bool, found: &mut Found) {
        match &e.kind {
            ExprKind::MapSet { local, .. } | ExprKind::MapRemove { local, .. } => {
                found.grown.insert(*local);
            }
            ExprKind::Index { base, .. } if looping => {
                if let (Some(key), Ty::Array(_) | Ty::Bytes) = (key_of(program, base), &base.ty) {
                    found.wanted.insert(key, base.ty != Ty::Bytes);
                }
            }
            _ => {}
        }
        for c in children(e) {
            expr(program, c, looping, found);
        }
    }
    fn block(program: &Program, types: &[Ty], b: &[Stmt], looping: bool, found: &mut Found) {
        for s in b {
            match s {
                Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(program, e, looping, found),
                Stmt::Assign(l, path, e, _) => {
                    let (mut ty, mut slot, mut first) = (&types[*l], 0, true);
                    for step in path {
                        match step {
                            Step::Field(i) if first => {
                                let Ty::Struct(sid) = ty else { unreachable!("field of {ty:?}") };
                                slot += crate::field_range(program, *sid, *i).0;
                                ty = &program.structs[*sid].fields[*i].1;
                            }
                            Step::Field(_) => {}
                            Step::Index { index, .. } => {
                                if first && looping && matches!(ty, Ty::Array(_)) {
                                    found.wanted.insert((*l, slot), true);
                                }
                                first = false;
                                expr(program, index, looping, found);
                            }
                        }
                    }
                    expr(program, e, looping, found);
                }
                Stmt::If(c, t, f) => {
                    expr(program, c, looping, found);
                    block(program, types, t, looping, found);
                    block(program, types, f, looping, found);
                }
                Stmt::Match { scrutinee, arms, .. } => {
                    expr(program, scrutinee, looping, found);
                    for arm in arms {
                        block(program, types, &arm.body, looping, found);
                    }
                }
                Stmt::While(c, body) => {
                    expr(program, c, true, found);
                    block(program, types, body, true, found);
                }
                Stmt::For(_, start, end, body) => {
                    expr(program, start, looping, found);
                    expr(program, end, looping, found);
                    block(program, types, body, true, found);
                }
                Stmt::ForEach { iter, body, .. } => {
                    expr(program, iter, looping, found);
                    block(program, types, body, true, found);
                }
                Stmt::Return(None) | Stmt::Drop(_) | Stmt::Break | Stmt::Continue | Stmt::Reached(_) | Stmt::Line(_) => {}
            }
        }
    }
    let mut found = Found::default();
    for c in f.requires.iter().chain(&f.ensures) {
        expr(program, &c.expr, false, &mut found);
    }
    block(program, types, &f.body, false, &mut found);
    found.wanted.retain(|key, _| !found.grown.contains(&key.0));
    found.wanted
}

fn is_innermost(body: &[Stmt]) -> bool {
    body.iter().all(|s| match s {
        Stmt::While(..) | Stmt::For(..) | Stmt::ForEach { .. } => false,
        Stmt::If(_, t, f) => is_innermost(t) && is_innermost(f),
        Stmt::Match { arms, .. } => arms.iter().all(|a| is_innermost(&a.body)),
        _ => true,
    })
}

/// Whether `e` is a straight line in `counter`: its value is a * counter + b for numbers the loop
/// does not change, and so are its parts, so each part lies between its values at the first and
/// the last round. Nothing in it can do anything but compute. `None` when it is not one; otherwise
/// whether it names the counter.
pub(crate) fn straight(e: &Expr, counter: LocalId, fixed: &dyn Fn(LocalId) -> bool) -> Option<bool> {
    match &e.kind {
        ExprKind::Int(_) => Some(false),
        ExprKind::Local(l) if *l == counter => Some(true),
        ExprKind::Local(l) => fixed(*l).then_some(false),
        ExprKind::Field(..) if e.ty == Ty::I64 => root(e).filter(|l| fixed(*l)).map(|_| false),
        ExprKind::Builtin(Builtin::Len, args) => root(&args[0]).filter(|l| fixed(*l)).map(|_| false),
        ExprKind::IArith { op, l, r, .. } => {
            let (a, b) = (straight(l, counter, fixed)?, straight(r, counter, fixed)?);
            match op {
                IOp::Add | IOp::Sub => Some(a || b),
                IOp::Mul if !(a && b) => Some(a || b),
                _ => None,
            }
        }
        _ => None,
    }
}

struct Find<'p, 'e, 'f> {
    program: &'p Program,
    types: &'p [Ty],
    counter: LocalId,
    fixed: &'f dyn Fn(LocalId) -> bool,
    out: Vec<Access<'e>>,
}

impl<'e> Find<'_, 'e, '_> {
    fn add(&mut self, key: Option<Key>, array: &Ty, index: &'e Expr, write: bool) {
        let elem = match array {
            Ty::Array(t) => Some((**t).clone()),
            Ty::Bytes => None,
            _ => return,
        };
        let Some(key) = key.filter(|k| (self.fixed)(k.0)) else { return };
        if straight(index, self.counter, self.fixed).is_some() {
            self.out.push(Access { key, elem, index, write });
        }
    }

    fn expr(&mut self, e: &'e Expr) {
        if let ExprKind::Index { base, index, .. } = &e.kind {
            self.add(key_of(self.program, base), &base.ty, index, false);
        }
        for c in children(e) {
            self.expr(c);
        }
    }

    /// The first index of a place reached through fields alone; indices after it are read.
    fn place(&mut self, l: LocalId, path: &'e [Step]) {
        let (mut ty, mut start) = (self.types[l].clone(), 0);
        let mut first = true;
        for step in path {
            match step {
                Step::Field(i) if first => {
                    let Ty::Struct(sid) = ty else { return };
                    start += crate::field_range(self.program, sid, *i).0;
                    ty = self.program.structs[sid].fields[*i].1.clone();
                }
                Step::Field(_) => {}
                Step::Index { index, .. } => {
                    if first {
                        self.add(Some((l, start)), &ty, index, true);
                        first = false;
                    }
                    self.expr(index);
                }
            }
        }
    }

    fn block(&mut self, b: &'e [Stmt]) {
        for s in b {
            match s {
                Stmt::Let(_, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => self.expr(e),
                Stmt::Assign(l, path, e, _) => {
                    self.place(*l, path);
                    self.expr(e);
                }
                Stmt::If(c, t, f) => {
                    self.expr(c);
                    self.block(t);
                    self.block(f);
                }
                Stmt::Match { scrutinee, arms, .. } => {
                    self.expr(scrutinee);
                    for arm in arms {
                        self.block(&arm.body);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The accesses in the body of a `for` loop with no loop inside whose array stays put and whose
/// index is a straight line in the counter: if the first and last round of each are in bounds,
/// every round's is.
pub(crate) fn clearable<'e>(program: &Program, types: &[Ty], counter: LocalId, body: &'e [Stmt]) -> Vec<Access<'e>> {
    if !is_innermost(body) {
        return Vec::new();
    }
    let mut changed = Changes::default();
    for s in body {
        changed.merge(changes(program, s));
    }
    if changed.touched.contains(&counter) {
        return Vec::new();
    }
    let gone = not_held(body);
    let fixed = |l: LocalId| !changed.touched.contains(&l) && !changed.shrunk.contains(&l) && !gone.contains(&l);
    let mut find = Find { program, types, counter, fixed: &fixed, out: Vec::new() };
    find.block(body);
    find.out
}

/// `while counter < limit [&& ...] { ...; counter = counter + step; }` with no loop inside, what
/// the counter's values allow to be tested before it starts.
pub(crate) struct CountedWhile<'e> {
    pub counter: LocalId,
    pub limit: &'e Expr,
    pub step: i64,
    /// `counter + step`: it cannot overflow once the last round is known not to.
    pub advance: &'e Expr,
    pub accesses: Vec<Access<'e>>,
}

/// The leftmost test of a chain of `&&`, and the others.
fn tests(e: &Expr) -> (&Expr, Vec<&Expr>) {
    match &e.kind {
        ExprKind::And(l, r) => {
            let (first, mut rest) = tests(l);
            rest.push(r);
            (first, rest)
        }
        _ => (e, Vec::new()),
    }
}

/// If the loop counts up by a fixed step to a limit it cannot change, as its first test and last
/// statement say. Its counter changes nowhere else, so every round's counter is one of
/// first, first + step, ... below the limit, whatever else the body does.
pub(crate) fn counted_while<'e>(program: &Program, types: &[Ty], cond: &'e Expr, body: &'e [Stmt]) -> Option<CountedWhile<'e>> {
    if !is_innermost(body) {
        return None;
    }
    let (first, rest) = tests(cond);
    let ExprKind::Cmp(CmpOp::Lt, l, limit) = &first.kind else { return None };
    let counter = local_of(l).filter(|_| l.ty == Ty::I64)?;
    let last = body.iter().rposition(|s| !matches!(s, Stmt::Line(_) | Stmt::Reached(_)))?;
    let Stmt::Assign(target, path, advance, _) = &body[last] else { return None };
    if *target != counter || !path.is_empty() {
        return None;
    }
    let ExprKind::IArith { op: IOp::Add, l: from, r: by, .. } = &advance.kind else { return None };
    let ExprKind::Int(step) = by.kind else { return None };
    if local_of(from) != Some(counter) || step <= 0 {
        return None;
    }
    let mut changed = Changes::default();
    for s in body.iter().take(last) {
        changed.merge(changes(program, s));
    }
    for t in &rest {
        let mut c = Changes::default();
        Walk { program, out: &mut c.touched, shrunk: Some(&mut c.shrunk) }.expr(t);
        changed.merge(c);
    }
    if changed.touched.contains(&counter) {
        return None;
    }
    let gone = not_held(body);
    let fixed = |l: LocalId| !changed.touched.contains(&l) && !changed.shrunk.contains(&l) && !gone.contains(&l);
    if straight(limit, counter, &fixed) != Some(false) {
        return None;
    }
    let mut find = Find { program, types, counter, fixed: &fixed, out: Vec::new() };
    for t in rest {
        find.expr(t);
    }
    find.block(body);
    Some(CountedWhile { counter, limit, step, advance, accesses: find.out })
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
