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
//! clauses read are never moved. Both backends follow the marks, so the rule is only here.

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

/// Marks the moves in every function of `program`, and lists the arguments of calls and whether
/// each moves (`Program::handed`). Tests are left as written.
pub fn mark(program: &mut Program) {
    let Program { funcs, structs, handed, .. } = program;
    for f in funcs.iter_mut() {
        let mut pinned = Live::new();
        for c in &f.ensures {
            refs(&c.expr, &mut pinned);
        }
        let walk = Walk { pinned, handed: RefCell::new(Vec::new()) };
        walk.block(&mut f.body, &Live::new(), None, true);
        for (site, local, path, kept) in walk.handed.into_inner() {
            let what = describe(&f.locals, structs, local, &path);
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

/// Every local `e` names, anywhere in it.
fn refs(e: &Expr, out: &mut Live) {
    if let Some(l) = named_local(e) {
        out.insert(l);
    }
    for c in children(e) {
        refs(c, out);
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

/// The locals `e` names and the places in them, in evaluation-independent walk order (the
/// order `apply` visits them in).
fn occs(e: &Expr, out: &mut Vec<Occ>, call: Option<&str>) {
    if let Some((local, path)) = place(e) {
        let call = call.filter(|_| costly(&e.ty)).map(str::to_string);
        out.push(Occ { local, path, read: true, call });
        for i in place_indices(e) {
            occs(i, out, None);
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
        out.push(Occ { local, path, read: false, call: None });
    }
    let site = match &e.kind {
        ExprKind::Call(_, _, site) | ExprKind::CallValue(_, _, site) => Some(site.as_str()),
        _ => None,
    };
    for c in children(e) {
        occs(c, out, site);
    }
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
            ExprKind::Index { base, index, site } => {
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

struct Walk {
    pinned: Live,
    /// Arguments: (call site, local, place, why it is not moved).
    handed: RefCell<Vec<(String, LocalId, Vec<Seg>, Option<&'static str>)>>,
}

impl Walk {
    /// What is live before `stmts`, given what is live after them; with `mark`, marks moves.
    fn block(&self, stmts: &mut [Stmt], after: &Live, lp: Option<&Loop>, mark: bool) -> Live {
        let mut live = after.clone();
        for s in stmts.iter_mut().rev() {
            live = self.stmt(s, &live, lp, mark);
        }
        live
    }

    /// Marks the moves of one statement that evaluates `exprs`. `target` is the place inside a
    /// local the statement writes after evaluating everything; `written` is the local it
    /// overwrites whole, whose old value is then dead.
    fn leaf(&self, exprs: Vec<&mut Expr>, target: Option<(LocalId, Vec<Seg>)>, written: Option<LocalId>, after: &Live) {
        let mut all = Vec::new();
        for e in &exprs {
            occs(e, &mut all, None);
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
        let others: Vec<&[Seg]> =
            all.iter().enumerate().filter(|(j, x)| *j != k && x.local == l).map(|(_, x)| &x.path[..]).collect();
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
            Stmt::Reached(_) => after.clone(),
            Stmt::If(c, a, b) => {
                let mut live = union(&self.block(a, after, lp, mark), &self.block(b, after, lp, mark));
                refs(c, &mut live);
                live
            }
            Stmt::Match { held, scrutinee, arms } => {
                let mut live = Live::new();
                for arm in arms.iter_mut() {
                    let mut l = self.block(&mut arm.body, after, lp, mark);
                    for (_, b) in &arm.bindings {
                        l.remove(b);
                    }
                    live.extend(l);
                }
                live.insert(*held);
                refs(scrutinee, &mut live);
                live
            }
            Stmt::While(c, body) => {
                let mut entry = after.clone();
                refs(c, &mut entry);
                self.looped(body, after, &entry, &[], mark)
            }
            // The counter and the snapshot are kept live through the loop, conservatively.
            Stmt::For(i, start, end, body) => {
                let mut live = self.looped(body, after, after, &[*i], mark);
                refs(start, &mut live);
                refs(end, &mut live);
                live
            }
            Stmt::ForEach { var, held, iter, body } => {
                let mut live = self.looped(body, after, after, &[*var, *held], mark);
                refs(iter, &mut live);
                live
            }
        }
    }

    /// What is live at the head of a loop whose body is `body`: the least set with
    /// head = base + kept + live before the body (after which the head comes again).
    fn looped(&self, body: &mut [Stmt], after: &Live, base: &Live, kept: &[LocalId], mark: bool) -> Live {
        let mut head = base.clone();
        head.extend(kept);
        loop {
            let lp = Loop { brk: after.clone(), cont: head.clone() };
            let next = union(&head, &self.block(body, &head, Some(&lp), false));
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
                    Stmt::If(_, a, b) => {
                        block(a, f, out);
                        block(b, f, out);
                    }
                    Stmt::While(_, b) | Stmt::For(_, _, _, b) | Stmt::ForEach { body: b, .. } => block(b, f, out),
                    Stmt::Match { arms, .. } => arms.iter().for_each(|a| block(&a.body, f, out)),
                    Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Reached(_) => {}
                }
            }
        }
        let p = crate::compile(&SourceFile::new("t.anv", src)).unwrap_or_else(|d| panic!("{d:?}"));
        let f = p.funcs.iter().find(|f| f.name == name).unwrap();
        let mut out = Vec::new();
        block(&f.body, f, &mut out);
        out
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
        assert_eq!(moved(&src, "build"), ["xs", "xs"]);
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
        // r[j] may be r[i]; c whole holds c.items; the element of a dead array is not taken.
        let src = format!(
            "{ADD}\n{BAG}\nfn other(rows: [[i64]], i: i64, j: i64) -> [[i64]] {{\n    var r: [[i64]] = rows;\n    r[i] = add(xs: r[j], x: 0);\n    return r;\n}}\n\n\
             fn whole(b: Bag) -> i64 {{\n    var c: Bag = b;\n    c.items = add(xs: c.items, x: c.items.len());\n    return c.n;\n}}\n\n\
             fn first(rows: [[i64]]) -> [i64] {{\n    return rows[0];\n}}\n"
        );
        assert_eq!(moved(&src, "other"), ["rows", "j", "r"]);
        assert_eq!(moved(&src, "whole"), ["b", "c.1"]);
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
                ("t.anv:13:20".to_string(), "b.items".to_string(), Some(super::NAMED_AGAIN)),
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

    #[test]
    fn a_value_named_twice_in_a_statement_stays() {
        let src = format!("{ADD}\nfn both(xs: [i64]) -> [i64] {{\n    return add(xs: xs, x: xs.len());\n}}\n");
        assert!(moved(&src, "both").is_empty());
    }
}
