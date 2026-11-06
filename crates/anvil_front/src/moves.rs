//! Moves: the last read of a local's value takes the value instead of sharing it, so a value
//! passed on (to a call, into a struct or an array, returned) keeps a single owner and can be
//! changed in place there. `xs = add(xs, x)` is then as cheap as `xs.push(x)`, not a copy of
//! `xs` per call.
//!
//! A read in a statement (`let`, an assignment, an expression statement, `return`) becomes a
//! `Move` when the statement names that local only once and nothing reads the local again
//! before it is next written, on any path (loops included). A read of a place inside a local
//! becomes a `Take` when nothing else in the statement names that place (or one around or
//! inside it) and either the statement writes the place again (`s.items = add(s.items, x)`,
//! `rows[i] = add(rows[i], x)`) or the local is dead after it and the place is reached by
//! fields alone (`s = State(items: add(s.items, x), n: s.n + 1)`). The values `ensures`
//! clauses read are never moved. Tests are marked like functions. Both backends follow the
//! marks, so the rule is only here.
//!
//! A local that still holds a value when nothing can read it again lets go of it there
//! (`Stmt::Drop`): after the statement that read it last, at the start of a branch that does not
//! read it, after a loop that did. So what it held does not wait for the function to end, and a
//! value also held elsewhere is not shared with a dead local when it is next changed.

use crate::ir::*;
use std::cell::RefCell;
use std::collections::BTreeSet;

type Live = BTreeSet<LocalId>;

/// Where `break` and `continue` go in the innermost loop: what is live there.
struct Loop {
    brk: Live,
    cont: Live,
}

/// Why a read is not a move, for the reads of call arguments `Program::handed` lists.
const ENSURED: &str = "an `ensures` clause reads it";
const NAMED_AGAIN: &str = "the statement names it again";
const WRITTEN_INTO: &str = "the statement writes into it";
const MAY_BE_WRITTEN: &str = "the statement writes a place that may be the same";
const READ_LATER: &str = "it is read again later";
const ELEMENT: &str = "an element is moved only when the statement writes it back";

/// Marks the moves in every function and test of `program`, and lists the arguments of calls
/// and whether each moves (`Program::handed`).
pub fn mark(program: &mut Program) {
    let Program { funcs, tests, structs, handed, .. } = program;
    let mut bodies: Vec<(Live, &[Local], &mut Vec<Stmt>)> = Vec::new();
    for f in funcs.iter_mut() {
        let mut pinned = Live::new();
        for c in &f.ensures {
            refs(&c.expr, &mut pinned);
        }
        bodies.push((pinned, &f.locals, &mut f.body));
    }
    for t in tests.iter_mut() {
        bodies.push((Live::new(), &t.locals, &mut t.body));
    }
    for (pinned, locals, body) in bodies {
        let walk = Walk { locals, pinned, handed: RefCell::new(Vec::new()) };
        walk.block(body, &Live::new(), None, true);
        for (site, local, path, kept) in walk.handed.into_inner() {
            let what = describe(locals, structs, local, &path);
            let list = handed.entry(site).or_default();
            let k = Handed { what, kept };
            // A generic's instances share its sites.
            if !list.contains(&k) {
                list.push(k);
            }
        }
    }
}

/// A place as written: `bag.items`, `rows[i]` (`rows[...]` for an index that is not a variable
/// or a literal).
fn describe(locals: &[Local], structs: &[StructDef], local: LocalId, path: &[Seg]) -> String {
    let mut out = locals[local].name.clone();
    let mut ty = locals[local].ty.clone();
    for seg in path {
        match (seg, ty.clone()) {
            (Seg::Field(i), Ty::Struct(sid)) => {
                let (name, t) = &structs[sid].fields[*i];
                out.push('.');
                out.push_str(name);
                ty = t.clone();
            }
            (Seg::Index(i), Ty::Array(elem)) => {
                match i {
                    Some(Simple::Local(l)) => out.push_str(&format!("[{}]", locals[*l].name)),
                    Some(Simple::Int(v)) => out.push_str(&format!("[{v}]")),
                    None => out.push_str("[...]"),
                }
                ty = *elem;
            }
            (_, t) => unreachable!("step into {t:?}"),
        }
    }
    out
}

/// Whether copying `ty` on a write can cost anything (text, bytes, arrays, maps, or a value
/// holding them): the arguments worth naming when they are not moved.
fn costly(ty: &Ty) -> bool {
    matches!(ty, Ty::Str | Ty::Bytes | Ty::Array(_) | Ty::Map(..) | Ty::Struct(_) | Ty::Enum(_))
}

/// Whether a local of type `ty` is worth letting go of: it may hold something on the heap.
fn holds(ty: &Ty) -> bool {
    costly(ty) || matches!(ty, Ty::Fn(_))
}

/// Every local `e` reads or changes, anywhere in it. The value `?` holds is written first.
fn refs(e: &Expr, out: &mut Live) {
    if let (Some(l), false) = (named_local(e), matches!(e.kind, ExprKind::Try { .. })) {
        out.insert(l);
    }
    for c in children(e) {
        refs(c, out);
    }
}

/// Every local that may hold a value once `e` has been evaluated, as far as `e` goes: those it
/// names, but for those it is sure to have moved out of.
fn left_holding(e: &Expr, out: &mut Live) {
    fn names(e: &Expr, out: &mut Live) {
        out.extend(named_local(e));
        for c in children(e) {
            names(c, out);
        }
    }
    let (mut named, mut gone) = (Live::new(), Live::new());
    names(e, &mut named);
    surely_moved(e, &mut gone);
    out.extend(named.difference(&gone));
}

/// The locals evaluating `e` is sure to move out of (the right of `&&` and `||` may not run).
fn surely_moved(e: &Expr, out: &mut Live) {
    match &e.kind {
        ExprKind::Move(l) => {
            out.insert(*l);
        }
        ExprKind::And(a, _) | ExprKind::Or(a, _) => surely_moved(a, out),
        ExprKind::Cond { cond, .. } => surely_moved(cond, out),
        _ => children(e).into_iter().for_each(|c| surely_moved(c, out)),
    }
}

/// One step into a place: a field, or an index, known (`Some`) when it is a local or a literal,
/// so that two such steps are the same element.
#[derive(Clone, Copy, PartialEq)]
enum Seg {
    Field(usize),
    Index(Option<Simple>),
}

#[derive(Clone, Copy, PartialEq)]
enum Simple {
    Local(LocalId),
    Int(i64),
}

/// A local a statement names, and the place inside it; `read` when the statement only reads
/// it there (so the read could move), not changing it in place or holding it. `call` is the
/// site of the call it is an argument of, when it is one whose copies cost anything.
struct Occ {
    local: LocalId,
    path: Vec<Seg>,
    read: bool,
    call: Option<String>,
    /// The call this read is a whole argument of.
    arg_of: Option<usize>,
    /// The calls around this occurrence.
    within: Vec<usize>,
}

/// What a read becomes.
#[derive(PartialEq)]
enum Act {
    Keep,
    /// The local's whole value moves (`Move` at the root of the place).
    Move,
    /// The value at the place is taken (`Take`).
    Take,
}

fn simple(e: &Expr) -> Option<Simple> {
    match e.kind {
        ExprKind::Local(l) => Some(Simple::Local(l)),
        ExprKind::Int(v) => Some(Simple::Int(v)),
        _ => None,
    }
}

fn segs(path: &[Step]) -> Vec<Seg> {
    path.iter()
        .map(|s| match s {
            Step::Field(i) => Seg::Field(*i),
            Step::Index { index, .. } => Seg::Index(simple(index)),
        })
        .collect()
}

/// `e` as a read of a place: its local and the steps to the place.
fn place(e: &Expr) -> Option<(LocalId, Vec<Seg>)> {
    match &e.kind {
        ExprKind::Local(l) => Some((*l, Vec::new())),
        ExprKind::Field(base, i) => {
            let (l, mut p) = place(base)?;
            p.push(Seg::Field(*i));
            Some((l, p))
        }
        ExprKind::Index { base, index, .. } if matches!(base.ty, Ty::Array(_)) => {
            let (l, mut p) = place(base)?;
            p.push(Seg::Index(simple(index)));
            Some((l, p))
        }
        _ => None,
    }
}

/// The index expressions of a place read, from its local outwards.
fn place_indices(e: &Expr) -> Vec<&Expr> {
    match &e.kind {
        ExprKind::Field(base, _) => place_indices(base),
        ExprKind::Index { base, index, .. } => {
            let mut out = place_indices(base);
            out.push(index);
            out
        }
        _ => Vec::new(),
    }
}

fn place_indices_mut(e: &mut Expr) -> Vec<&mut Expr> {
    match &mut e.kind {
        ExprKind::Field(base, _) => place_indices_mut(base),
        ExprKind::Index { base, index, .. } => {
            let mut out = place_indices_mut(base);
            out.push(index);
            out
        }
        _ => Vec::new(),
    }
}

/// Where `occs` is: the call whose argument it is walking, and the calls around it.
#[derive(Default)]
struct OccWalk {
    calls: usize,
    within: Vec<usize>,
}

/// The locals `e` names and the places in them, in evaluation-independent walk order (the
/// order `apply` visits them in).
fn occs(e: &Expr, out: &mut Vec<Occ>, call: Option<&str>, arg_of: Option<usize>, walk: &mut OccWalk) {
    if let Some((local, path)) = place(e) {
        let call = call.filter(|_| costly(&e.ty)).map(str::to_string);
        out.push(Occ { local, path, read: true, call, arg_of, within: walk.within.clone() });
        for i in place_indices(e) {
            occs(i, out, None, None, walk);
        }
        return;
    }
    if let Some(local) = named_local(e) {
        let path = match &e.kind {
            ExprKind::Push { path, .. }
            | ExprKind::Pop { path, .. }
            | ExprKind::MapSet { path, .. }
            | ExprKind::MapRemove { path, .. } => segs(path),
            _ => Vec::new(),
        };
        out.push(Occ { local, path, read: false, call: None, arg_of: None, within: walk.within.clone() });
    }
    let site = match &e.kind {
        ExprKind::Call(_, _, site) | ExprKind::CallValue(_, _, site) => Some(site.as_str()),
        _ => None,
    };
    let Some(_) = site else {
        for c in children(e) {
            occs(c, out, None, None, walk);
        }
        return;
    };
    walk.calls += 1;
    let id = walk.calls;
    walk.within.push(id);
    // What a function value is called through is read first, wherever its arguments go.
    let callee = matches!(e.kind, ExprKind::CallValue(..));
    for (i, c) in children(e).into_iter().enumerate() {
        occs(c, out, site, (!callee || i > 0).then_some(id), walk);
    }
    walk.within.pop();
}

/// Whether two places in one local can share a value: one is around the other, unless a field
/// tells them apart (indices may always be the same element).
fn overlap(a: &[Seg], b: &[Seg]) -> bool {
    a.iter().zip(b).all(|(x, y)| !matches!((x, y), (Seg::Field(i), Seg::Field(j)) if i != j))
}

/// Whether place `t` is `p` or around it, by steps known to be the same.
fn covers(t: &[Seg], p: &[Seg]) -> bool {
    t.len() <= p.len() && t.iter().zip(p).all(|(x, y)| x == y && *x != Seg::Index(None))
}

fn by_fields(p: &[Seg]) -> bool {
    p.iter().all(|s| matches!(s, Seg::Field(_)))
}

/// Applies `acts` to the reads of `e`, visiting them in `occs` order.
fn apply(e: &mut Expr, acts: &[Act], k: &mut usize) {
    if place(e).is_some() {
        let act = &acts[*k];
        *k += 1;
        match act {
            Act::Take => {
                take(e);
                let ExprKind::Take { path, .. } = &mut e.kind else { unreachable!() };
                for step in path {
                    if let Step::Index { index, .. } = step {
                        apply(index, acts, k);
                    }
                }
                return;
            }
            Act::Move => {
                let root = root_mut(e);
                let ExprKind::Local(l) = root.kind else { unreachable!("place without a local") };
                root.kind = ExprKind::Move(l);
            }
            Act::Keep => {}
        }
        for i in place_indices_mut(e) {
            apply(i, acts, k);
        }
        return;
    }
    if named_local(e).is_some() {
        *k += 1;
    }
    for c in children_mut(e) {
        apply(c, acts, k);
    }
}

/// The local at the root of a place read.
fn root_mut(e: &mut Expr) -> &mut Expr {
    if !matches!(e.kind, ExprKind::Field(..) | ExprKind::Index { .. }) {
        return e;
    }
    match &mut e.kind {
        ExprKind::Field(base, _) | ExprKind::Index { base, .. } => root_mut(base),
        _ => unreachable!(),
    }
}

/// Turns a place read into a `Take` of the same place.
fn take(e: &mut Expr) {
    let mut path = Vec::new();
    let mut cur = std::mem::replace(&mut e.kind, ExprKind::Int(0));
    let local = loop {
        match cur {
            ExprKind::Local(l) => break l,
            ExprKind::Field(base, i) => {
                path.push(Step::Field(i));
                cur = base.kind;
            }
            ExprKind::Index { base, index, site, .. } => {
                path.push(Step::Index { index: *index, site });
                cur = base.kind;
            }
            other => unreachable!("take of {other:?}"),
        }
    };
    path.reverse();
    e.kind = ExprKind::Take { local, path };
}

fn union(a: &Live, b: &Live) -> Live {
    a.union(b).copied().collect()
}

struct Walk<'a> {
    locals: &'a [Local],
    pinned: Live,
    /// Arguments: (call site, local, place, why it is not moved).
    handed: RefCell<Vec<(String, LocalId, Vec<Seg>, Option<&'static str>)>>,
}

impl Walk<'_> {
    /// What is live before `stmts`, given what is live after them; with `mark`, marks moves and
    /// adds drops.
    fn block(&self, stmts: &mut Vec<Stmt>, after: &Live, lp: Option<&Loop>, mark: bool) -> Live {
        let mut live = after.clone();
        if !mark {
            for s in stmts.iter_mut().rev() {
                live = self.stmt(s, &live, lp, false);
            }
            return live;
        }
        let mut out = Vec::with_capacity(stmts.len());
        for mut s in std::mem::take(stmts).into_iter().rev() {
            let before = self.stmt(&mut s, &live, lp, true);
            let held = holding_after(&s, &before);
            out.extend(self.drops(&held, &live).into_iter().rev());
            out.push(s);
            live = before;
        }
        out.reverse();
        *stmts = out;
        live
    }

    /// Drops of those of `held` not in `live`, that are worth one.
    fn drops(&self, held: &Live, live: &Live) -> Vec<Stmt> {
        held.difference(live)
            .filter(|l| !self.pinned.contains(l) && holds(&self.locals[**l].ty))
            .map(|l| Stmt::Drop(*l))
            .collect()
    }

    /// Starts a branch by letting go of those of `held` it does not read (`live` at its start).
    fn start_with_drops(&self, branch: &mut Vec<Stmt>, held: &Live, live: &Live) {
        branch.splice(0..0, self.drops(held, live));
    }

    /// Marks the moves of one statement that evaluates `exprs`. `target` is the place inside a
    /// local the statement writes after evaluating everything; `written` is the local it
    /// overwrites whole, whose old value is then dead.
    fn leaf(&self, exprs: Vec<&mut Expr>, target: Option<(LocalId, Vec<Seg>)>, written: Option<LocalId>, after: &Live) {
        let mut all = Vec::new();
        let mut walk = OccWalk::default();
        for e in &exprs {
            occs(e, &mut all, None, None, &mut walk);
        }
        let (acts, whys): (Vec<Act>, Vec<&'static str>) =
            (0..all.len()).map(|k| self.act(&all, k, target.as_ref(), written, after)).unzip();
        for (o, (act, why)) in all.iter().zip(acts.iter().zip(&whys)) {
            if let (Some(site), true) = (&o.call, o.read) {
                let kept = (*act == Act::Keep).then_some(*why);
                self.handed.borrow_mut().push((site.clone(), o.local, o.path.clone(), kept));
            }
        }
        if acts.iter().any(|a| *a != Act::Keep) {
            let mut k = 0;
            for e in exprs {
                apply(e, &acts, &mut k);
            }
        }
    }

    /// What read `all[k]` of a statement becomes, and why when it is kept.
    fn act(
        &self,
        all: &[Occ],
        k: usize,
        target: Option<&(LocalId, Vec<Seg>)>,
        written: Option<LocalId>,
        after: &Live,
    ) -> (Act, &'static str) {
        let o = &all[k];
        if !o.read {
            return (Act::Keep, "");
        }
        if self.pinned.contains(&o.local) {
            return (Act::Keep, ENSURED);
        }
        let l = o.local;
        let dead = !after.contains(&l) || written == Some(l);
        // A whole argument handed over comes last (`ir::arg_comes_last`), so what the call's other
        // arguments read of it has been read by then.
        let last_in = o.arg_of.filter(|_| by_fields(&o.path));
        let read_before = |x: &Occ| last_in.is_some_and(|c| x.read && x.arg_of != Some(c) && x.within.contains(&c));
        let others: Vec<&[Seg]> = all
            .iter()
            .enumerate()
            .filter(|(j, x)| *j != k && x.local == l && !read_before(x))
            .map(|(_, x)| &x.path[..])
            .collect();
        let target = target.filter(|(t, _)| *t == l).map(|(_, p)| &p[..]);
        let alone = others.is_empty() && target.is_none();
        let apart = others.iter().all(|p| !overlap(p, &o.path));
        if !o.path.is_empty() && apart {
            // Written again by the statement: taken along the steps the write takes anyway, and
            // fields from there (an index past them would copy a shared array for nothing).
            if let Some(t) = target {
                if covers(t, &o.path) && by_fields(&o.path[t.len()..]) {
                    return (Act::Take, "");
                }
            }
            if dead && by_fields(&o.path) && target.is_none_or(|t| !overlap(t, &o.path)) {
                return (Act::Take, "");
            }
        }
        if alone && dead {
            return (Act::Move, "");
        }
        let why = if !apart {
            NAMED_AGAIN
        } else if target.is_some_and(|t| overlap(t, &o.path)) {
            if o.path.is_empty() { WRITTEN_INTO } else { MAY_BE_WRITTEN }
        } else if !dead {
            READ_LATER
        } else {
            ELEMENT
        };
        (Act::Keep, why)
    }

    fn stmt(&self, s: &mut Stmt, after: &Live, lp: Option<&Loop>, mark: bool) -> Live {
        match s {
            Stmt::Let(l, e) => {
                let mut live = after.clone();
                live.remove(l);
                refs(e, &mut live);
                if mark {
                    self.leaf(vec![e], None, Some(*l), after);
                }
                live
            }
            Stmt::Assign(l, path, e, _) => {
                let mut live = after.clone();
                // Appending in place reads the local itself (see `append_parts`): left as is.
                let append = path.is_empty() && append_parts(*l, e).is_some();
                if path.is_empty() {
                    live.remove(l);
                } else {
                    live.insert(*l);
                }
                refs(e, &mut live);
                for step in path.iter() {
                    if let Step::Index { index, .. } = step {
                        refs(index, &mut live);
                    }
                }
                if mark && !append {
                    let (target, written) = if path.is_empty() { (None, Some(*l)) } else { (Some((*l, segs(path))), None) };
                    let mut exprs = vec![e];
                    for step in path.iter_mut() {
                        if let Step::Index { index, .. } = step {
                            exprs.push(index);
                        }
                    }
                    self.leaf(exprs, target, written, after);
                }
                live
            }
            Stmt::Expr(e) => {
                let mut live = after.clone();
                refs(e, &mut live);
                if mark {
                    self.leaf(vec![e], None, None, after);
                }
                live
            }
            Stmt::Return(e) => {
                let mut live = Live::new();
                if let Some(e) = e {
                    refs(e, &mut live);
                    if mark {
                        self.leaf(vec![e], None, None, &Live::new());
                    }
                }
                live
            }
            Stmt::Break => lp.expect("break outside a loop").brk.clone(),
            Stmt::Continue => lp.expect("continue outside a loop").cont.clone(),
            Stmt::Reached(_) | Stmt::Drop(_) | Stmt::Line(_) => after.clone(),
            Stmt::If(c, a, b) => {
                let (in_a, in_b) = (self.block(a, after, lp, mark), self.block(b, after, lp, mark));
                let mut live = union(&in_a, &in_b);
                if mark {
                    self.leaf(vec![c], None, None, &live);
                    let mut held = live.clone();
                    left_holding(c, &mut held);
                    self.start_with_drops(a, &held, &in_a);
                    self.start_with_drops(b, &held, &in_b);
                }
                refs(c, &mut live);
                live
            }
            // Each arm's bindings are copied out of `held` before its body, and nothing else
            // reads it.
            Stmt::Match { held, scrutinee, arms } => {
                let mut live = Live::new();
                let mut starts = Vec::new();
                for arm in arms.iter_mut() {
                    let start = self.block(&mut arm.body, after, lp, mark);
                    let mut l = start.clone();
                    for (_, b) in &arm.bindings {
                        l.remove(b);
                    }
                    live.extend(l);
                    starts.push(start);
                }
                if mark {
                    self.leaf(vec![scrutinee], None, None, &live);
                    let mut there = live.clone();
                    there.insert(*held);
                    left_holding(scrutinee, &mut there);
                    for (arm, start) in arms.iter_mut().zip(&starts) {
                        let mut h = there.clone();
                        h.extend(arm.bindings.iter().map(|(_, b)| *b));
                        self.start_with_drops(&mut arm.body, &h, start);
                    }
                }
                refs(scrutinee, &mut live);
                live
            }
            Stmt::While(c, body) => {
                let mut entry = after.clone();
                refs(c, &mut entry);
                self.looped(body, after, &entry, &[], None, mark)
            }
            // The counter is kept live through the loop, conservatively.
            Stmt::For(i, start, end, body) => {
                let mut live = self.looped(body, after, after, &[*i], None, mark);
                if mark {
                    self.leaf(vec![start, end], None, None, &live);
                }
                live.remove(i);
                refs(start, &mut live);
                refs(end, &mut live);
                live
            }
            Stmt::ForEach { var, held, iter, body } => {
                let mut live = self.looped(body, after, after, &[*held], Some(*var), mark);
                if mark {
                    self.leaf(vec![iter], None, None, &live);
                }
                live.remove(var);
                live.remove(held);
                refs(iter, &mut live);
                live
            }
        }
    }

    /// What is live at the head of a loop whose body is `body`: the least set with
    /// head = base + kept + live before the body (after which the head comes again). `rebound` is
    /// written at the head of each round, so what the body read of it last can move.
    fn looped(
        &self,
        body: &mut Vec<Stmt>,
        after: &Live,
        base: &Live,
        kept: &[LocalId],
        rebound: Option<LocalId>,
        mark: bool,
    ) -> Live {
        let mut head = base.clone();
        head.extend(kept);
        loop {
            let lp = Loop { brk: after.clone(), cont: head.clone() };
            let mut next = union(&head, &self.block(body, &head, Some(&lp), false));
            next.retain(|l| Some(*l) != rebound);
            if next == head {
                break;
            }
            head = next;
        }
        if mark {
            let lp = Loop { brk: after.clone(), cont: head.clone() };
            self.block(body, &head, Some(&lp), true);
        }
        head
    }
}

/// The locals that may hold a value once `s` has run, given what is live before it; those not
/// live after it can let go. A branch's are dropped at its start instead, and nothing follows a
/// jump.
fn holding_after(s: &Stmt, before: &Live) -> Live {
    let mut out = Live::new();
    match s {
        Stmt::Let(l, e) => {
            out.insert(*l);
            left_holding(e, &mut out);
        }
        Stmt::Assign(l, path, e, _) => {
            out.insert(*l);
            left_holding(e, &mut out);
            for step in path {
                if let Step::Index { index, .. } = step {
                    left_holding(index, &mut out);
                }
            }
        }
        Stmt::Expr(e) => left_holding(e, &mut out),
        Stmt::While(c, _) => {
            out.extend(before);
            left_holding(c, &mut out);
        }
        Stmt::For(_, start, end, _) => {
            let mut gone = Live::new();
            surely_moved(start, &mut gone);
            surely_moved(end, &mut gone);
            out.extend(before.difference(&gone));
            left_holding(start, &mut out);
            left_holding(end, &mut out);
        }
        Stmt::ForEach { var, held, iter, .. } => {
            let mut gone = Live::new();
            surely_moved(iter, &mut gone);
            out.extend(before.difference(&gone));
            out.extend([*var, *held]);
            left_holding(iter, &mut out);
        }
        Stmt::If(..)
        | Stmt::Match { .. }
        | Stmt::Return(_)
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Reached(_)
        | Stmt::Drop(_)
        | Stmt::Line(_) => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::ir::*;
    use crate::span::SourceFile;

    /// The locals whose reads in function `name` are moves, in the order they appear; takes as
    /// the local and their steps (`s.0` for field 0, `s[]` for an element).
    fn moved(src: &str, name: &str) -> Vec<String> {
        fn expr(e: &Expr, f: &Func, out: &mut Vec<String>) {
            match &e.kind {
                ExprKind::Move(l) => out.push(f.locals[*l].name.clone()),
                ExprKind::Take { local, path } => {
                    let mut s = f.locals[*local].name.clone();
                    for step in path {
                        match step {
                            Step::Field(i) => s.push_str(&format!(".{i}")),
                            Step::Index { .. } => s.push_str("[]"),
                        }
                    }
                    out.push(s);
                }
                _ => {}
            }
            for c in children(e) {
                expr(c, f, out);
            }
        }
        fn block(stmts: &[Stmt], f: &Func, out: &mut Vec<String>) {
            for s in stmts {
                match s {
                    Stmt::Let(_, e) | Stmt::Assign(_, _, e, _) | Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, f, out),
                    Stmt::If(c, a, b) => {
                        expr(c, f, out);
                        block(a, f, out);
                        block(b, f, out);
                    }
                    Stmt::While(_, b) => block(b, f, out),
                    Stmt::For(_, start, end, b) => {
                        expr(start, f, out);
                        expr(end, f, out);
                        block(b, f, out);
                    }
                    Stmt::ForEach { iter, body: b, .. } => {
                        expr(iter, f, out);
                        block(b, f, out);
                    }
                    Stmt::Match { scrutinee, arms, .. } => {
                        expr(scrutinee, f, out);
                        arms.iter().for_each(|a| block(&a.body, f, out));
                    }
                    Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Reached(_) | Stmt::Drop(_) | Stmt::Line(_) => {}
                }
            }
        }
        let p = crate::compile(&SourceFile::new("t.anv", src)).unwrap_or_else(|d| panic!("{d:?}"));
        let f = p.funcs.iter().find(|f| f.name == name).unwrap();
        let mut out = Vec::new();
        block(&f.body, f, &mut out);
        out
    }

    /// Function `name`'s statements as a line each, indented by depth: the drops by the local's
    /// name, anything else by what it is.
    fn shape(src: &str, name: &str) -> Vec<String> {
        fn block(stmts: &[Stmt], f: &Func, depth: usize, out: &mut Vec<String>) {
            for s in stmts {
                let pad = "  ".repeat(depth);
                match s {
                    // Hidden locals are numbered, and `$` is enough here.
                    Stmt::Drop(l) => {
                        let name = &f.locals[*l].name;
                        out.push(format!("{pad}drop {}", if name.starts_with('$') { "$" } else { name }));
                    }
                    Stmt::Let(l, _) => out.push(format!("{pad}let {}", f.locals[*l].name)),
                    Stmt::Assign(l, ..) => out.push(format!("{pad}set {}", f.locals[*l].name)),
                    Stmt::Expr(_) => out.push(format!("{pad}expr")),
                    Stmt::Return(_) => out.push(format!("{pad}return")),
                    Stmt::Break => out.push(format!("{pad}break")),
                    Stmt::Continue => out.push(format!("{pad}continue")),
                    Stmt::Reached(_) | Stmt::Line(_) => {}
                    Stmt::If(_, a, b) => {
                        out.push(format!("{pad}if"));
                        block(a, f, depth + 1, out);
                        out.push(format!("{pad}else"));
                        block(b, f, depth + 1, out);
                    }
                    Stmt::While(_, b) | Stmt::For(_, _, _, b) | Stmt::ForEach { body: b, .. } => {
                        out.push(format!("{pad}loop"));
                        block(b, f, depth + 1, out);
                    }
                    Stmt::Match { arms, .. } => {
                        for a in arms {
                            out.push(format!("{pad}arm"));
                            block(&a.body, f, depth + 1, out);
                        }
                    }
                }
            }
        }
        let p = crate::compile(&SourceFile::new("t.anv", src)).unwrap_or_else(|d| panic!("{d:?}"));
        let f = p.funcs.iter().find(|f| f.name == name).unwrap();
        let mut out = Vec::new();
        block(&f.body, f, 0, &mut out);
        out
    }

    #[test]
    fn a_dead_local_lets_go() {
        // After its last read, when that read cannot move it (named twice), and a value never read.
        let src = "fn f(xs: [i64]) -> i64 {\n    let a: i64 = xs[0] + xs[1];\n    let _unused: [i64] = [a];\n    return a;\n}\n";
        assert_eq!(shape(src, "f"), ["let a", "drop xs", "let _unused", "drop _unused", "return"]);
        // A read of an element moves the array in, which lets go of it there: no drop.
        let src = "fn f(rows: [[i64]]) -> [i64] {\n    let first: [i64] = rows[0];\n    return first;\n}\n";
        assert_eq!(shape(src, "f"), ["let first", "return"]);
        // A move needs no drop.
        assert_eq!(shape(ADD, "add"), ["let ys", "expr", "return"]);
    }

    #[test]
    fn a_branch_lets_go_of_what_it_does_not_read() {
        let src = "fn f(xs: [i64], c: bool) -> i64 {\n    if c {\n        return xs.len();\n    }\n    return 0;\n}\n";
        assert_eq!(shape(src, "f"), ["if", "  return", "else", "  drop xs", "return"]);
    }

    #[test]
    fn a_condition_moves_what_is_dead_after_it() {
        let src = "fn f(xs: [i64]) -> i64 {\n    if xs.len() > 0 {\n        return 1;\n    }\n    return 0;\n}\n";
        assert_eq!(moved(src, "f"), ["xs"]);
        assert_eq!(shape(src, "f"), ["if", "  return", "else", "return"]);
        // What a `for` or a `match` is over, likewise.
        let src = "fn f(xs: [i64]) -> i64 {\n    var t: i64 = 0;\n    for x in xs {\n        t = t + x;\n    }\n    return t;\n}\n";
        assert_eq!(moved(src, "f"), ["xs", "t", "x", "t"]);
        let src = "fn f(o: Option[[i64]]) -> i64 {\n    match o {\n        Some(value) => { return value.len(); }\n        None => { return 0; }\n    }\n}\n";
        assert_eq!(moved(src, "f"), ["o", "value"]);
    }

    #[test]
    fn a_loop_lets_go_after_it_and_keeps_what_it_reads() {
        let src = "fn f(xs: [i64], n: i64) -> i64 {\n    var t: i64 = 0;\n    for _i in 0..n {\n        t = t + xs.len();\n    }\n    return t;\n}\n";
        assert_eq!(shape(src, "f"), ["let t", "loop", "  set t", "drop xs", "return"]);
        // The snapshot goes after the loop, and a value made in the body within it.
        let src = "fn f(xs: [i64]) -> i64 {\n    var t: i64 = 0;\n    for x in xs {\n        let a: [i64] = [x];\n        t = t + a[0] + a[0];\n    }\n    return t;\n}\n";
        assert_eq!(shape(src, "f"), ["let t", "loop", "  let a", "  set t", "  drop a", "drop $", "return"]);
    }

    #[test]
    fn a_match_lets_go_of_the_scrutinee_in_each_arm() {
        let src = "fn f(o: Option[[i64]]) -> i64 {\n    match o {\n        Some(value) => { return value.len(); }\n        None => { return 0; }\n    }\n}\n";
        assert_eq!(shape(src, "f"), ["arm", "  drop $", "  return", "arm", "  drop $", "  return"]);
    }

    #[test]
    fn what_ensures_reads_is_not_dropped() {
        let src = "fn f(xs: [i64]) -> i64\n    ensures result == xs.len()\n{\n    let a: i64 = xs.len() + xs.len() - xs.len();\n    return a;\n}\n";
        assert_eq!(shape(src, "f"), ["let a", "return"]);
    }

    const ADD: &str = "fn add(xs: [i64], x: i64) -> [i64] {\n    var ys: [i64] = xs;\n    ys.push(x);\n    return ys;\n}\n";

    #[test]
    fn last_reads_move() {
        assert_eq!(moved(ADD, "add"), ["xs", "x", "ys"]);
    }

    #[test]
    fn an_update_in_a_loop_moves_but_the_counter_does_not() {
        let src = format!(
            "{ADD}\nfn build(n: i64) -> [i64] {{\n    var xs: [i64] = [];\n    for i in 0..n {{\n        xs = add(xs: xs, x: i);\n    }}\n    return xs;\n}}\n"
        );
        assert_eq!(moved(&src, "build"), ["n", "xs", "xs"]);
    }

    #[test]
    fn a_value_read_later_stays() {
        let src = format!(
            "{ADD}\nfn twice(xs: [i64]) -> i64 {{\n    let a: [i64] = add(xs: xs, x: 1);\n    return a.len() + xs.len();\n}}\n"
        );
        assert_eq!(moved(&src, "twice"), ["a", "xs"]);
    }

    #[test]
    fn a_value_read_on_the_next_round_stays() {
        let src = format!(
            "{ADD}\nfn rounds(xs: [i64]) -> i64 {{\n    var n: i64 = 0;\n    while n < 3 {{\n        let t: [i64] = add(xs: xs, x: n);\n        n = n + t.len();\n    }}\n    return n;\n}}\n"
        );
        assert_eq!(moved(&src, "rounds"), ["n", "t", "n"]);
    }

    #[test]
    fn what_ensures_reads_stays() {
        let src = format!(
            "{ADD}\nfn longer(xs: [i64]) -> [i64]\n    ensures result.len() == xs.len() + 1\n{{\n    return add(xs: xs, x: 0);\n}}\n"
        );
        assert!(moved(&src, "longer").is_empty());
    }

    #[test]
    fn the_last_read_of_a_loop_variable_moves_it() {
        let src = "fn f(xs: [[i64]]) -> [[i64]] {\n    var out: [[i64]] = [];\n    for x in xs {\n        out.push(x);\n    }\n    return out;\n}\n";
        assert_eq!(moved(src, "f"), ["xs", "x", "out"]);
    }

    #[test]
    fn an_argument_read_by_another_is_handed_over_last() {
        let src = format!("{ADD}\nfn f(xs: [i64]) -> [i64] {{\n    return add(xs: xs, x: xs.len());\n}}\n");
        assert_eq!(moved(&src, "f"), ["xs"]);
        // Read again after the call, or by a call around it: it stays.
        let src = format!(
            "{ADD}\nfn pair(a: [i64], b: i64) -> i64 {{\n    return a.len() + b;\n}}\n\nfn f(xs: [i64]) -> i64 {{\n    return pair(a: add(xs: xs, x: 1), b: xs.len());\n}}\n"
        );
        assert!(!moved(&src, "f").contains(&"xs".to_string()), "{:?}", moved(&src, "f"));
        // Named whole twice: one of the two would see the other's move.
        let src = format!(
            "fn same(a: [i64], b: [i64]) -> i64 {{\n    return a.len() + b.len();\n}}\n\nfn f(xs: [i64]) -> i64 {{\n    return same(a: xs, b: xs);\n}}\n"
        );
        assert!(moved(&src, "f").is_empty(), "{:?}", moved(&src, "f"));
    }

    const BAG: &str = "struct Bag {\n    items: [i64],\n    n: i64,\n}\n";

    #[test]
    fn a_field_written_again_is_taken() {
        let src = format!(
            "{ADD}\n{BAG}\nfn fill(b: Bag) -> Bag {{\n    var c: Bag = b;\n    c.items = add(xs: c.items, x: c.n);\n    return c;\n}}\n"
        );
        assert_eq!(moved(&src, "fill"), ["b", "c.0", "c"]);
    }

    #[test]
    fn fields_of_a_dead_value_are_taken_apart() {
        let src = format!(
            "{ADD}\n{BAG}\nfn bump(b: Bag) -> Bag {{\n    return Bag {{ items: add(xs: b.items, x: 1), n: b.n + 1 }};\n}}\n"
        );
        assert_eq!(moved(&src, "bump"), ["b.0", "b.1"]);
    }

    #[test]
    fn an_element_written_again_is_taken() {
        let src = format!(
            "{ADD}\nfn grow(rows: [[i64]], i: i64) -> [[i64]] {{\n    var r: [[i64]] = rows;\n    r[i] = add(xs: r[i], x: 0);\n    return r;\n}}\n"
        );
        assert_eq!(moved(&src, "grow"), ["rows", "r[]", "r"]);
    }

    #[test]
    fn places_that_may_be_the_same_stay() {
        // r[j] may be r[i]; the element of a dead array is not taken. c.items is taken: its other
        // read comes first.
        let src = format!(
            "{ADD}\n{BAG}\nfn other(rows: [[i64]], i: i64, j: i64) -> [[i64]] {{\n    var r: [[i64]] = rows;\n    r[i] = add(xs: r[j], x: 0);\n    return r;\n}}\n\n\
             fn whole(b: Bag) -> i64 {{\n    var c: Bag = b;\n    c.items = add(xs: c.items, x: c.items.len());\n    return c.n;\n}}\n\n\
             fn first(rows: [[i64]]) -> [i64] {{\n    return rows[0];\n}}\n"
        );
        assert_eq!(moved(&src, "other"), ["rows", "j", "r"]);
        assert_eq!(moved(&src, "whole"), ["b", "c.0", "c.1"]);
        assert_eq!(moved(&src, "first"), ["rows"]);
    }

    #[test]
    fn arguments_not_moved_say_why() {
        let src = format!(
            "{ADD}\n{BAG}\nfn keeps(b: Bag, rows: [[i64]], i: i64, j: i64) -> i64 {{\n    let a: [i64] = add(xs: b.items, x: b.items.len());\n\
             \x20   var r: [[i64]] = rows;\n    r[i] = add(xs: r[j], x: 0);\n    let c: [i64] = add(xs: r[0], x: 1);\n\
             \x20   return a.len() + c.len() + r.len() + b.n;\n}}\n"
        );
        let p = crate::compile(&SourceFile::new("t.anv", src)).unwrap_or_else(|d| panic!("{d:?}"));
        let handed: Vec<(String, String, Option<&str>)> = p
            .handed
            .iter()
            .filter(|(site, _)| site.starts_with("t.anv:1"))
            .flat_map(|(site, ks)| ks.iter().map(move |k| (site.clone(), k.what.clone(), k.kept)))
            .collect();
        assert_eq!(
            handed,
            [
                ("t.anv:13:20".to_string(), "b.items".to_string(), Some(super::READ_LATER)),
                ("t.anv:15:12".to_string(), "r[j]".to_string(), Some(super::MAY_BE_WRITTEN)),
                ("t.anv:16:20".to_string(), "r[0]".to_string(), Some(super::READ_LATER)),
            ]
        );
        // In add, `xs` is handed to nothing; in keeps, a moved argument is listed as moved.
        let src = format!("{ADD}\nfn twice(xs: [i64]) -> [i64] {{\n    return add(xs: add(xs: xs, x: 1), x: 2);\n}}\n");
        let p = crate::compile(&SourceFile::new("t.anv", src)).unwrap_or_else(|d| panic!("{d:?}"));
        let all: Vec<(&str, Option<&str>)> = p.handed.values().flatten().map(|k| (k.what.as_str(), k.kept)).collect();
        assert_eq!(all, [("xs", None)]);
    }
}
