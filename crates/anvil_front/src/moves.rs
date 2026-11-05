//! Moves: the last read of a local's value takes the value instead of sharing it, so a value
//! passed on (to a call, into a struct or an array, returned) keeps a single owner and can be
//! changed in place there. `xs = add(xs, x)` is then as cheap as `xs.push(x)`, not a copy of
//! `xs` per call.
//!
//! A read in a statement (`let`, an assignment, an expression statement, `return`) becomes a
//! `Move` when the statement names that local only once and nothing reads the local again
//! before it is next written, on any path (loops included). The values `ensures` clauses read
//! are never moved. Both backends follow the marks, so the rule is only here.

use crate::ir::*;
use std::collections::BTreeSet;

type Live = BTreeSet<LocalId>;

/// Where `break` and `continue` go in the innermost loop: what is live there.
struct Loop {
    brk: Live,
    cont: Live,
}

/// Marks the moves in every function of `program`. Tests are left as written.
pub fn mark(program: &mut Program) {
    for f in &mut program.funcs {
        let mut pinned = Live::new();
        for c in &f.ensures {
            refs(&c.expr, &mut pinned);
        }
        Walk { pinned }.block(&mut f.body, &Live::new(), None, true);
    }
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

/// How many times each local is named in `e`, added to `counts`.
fn count(e: &Expr, counts: &mut Vec<(LocalId, usize)>) {
    if let Some(l) = named_local(e) {
        match counts.iter_mut().find(|(x, _)| *x == l) {
            Some((_, n)) => *n += 1,
            None => counts.push((l, 1)),
        }
    }
    for c in children(e) {
        count(c, counts);
    }
}

fn rewrite(e: &mut Expr, moved: &Live) {
    if let ExprKind::Local(l) = e.kind {
        if moved.contains(&l) {
            e.kind = ExprKind::Move(l);
        }
        return;
    }
    for c in children_mut(e) {
        rewrite(c, moved);
    }
}

fn union(a: &Live, b: &Live) -> Live {
    a.union(b).copied().collect()
}

struct Walk {
    pinned: Live,
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

    /// Marks the moves of one statement that evaluates `exprs`. `place` is a local the
    /// statement also names (the target of an assignment to a place inside it); `written` is
    /// the local it overwrites after evaluating everything, whose old value is then dead.
    fn leaf(&self, exprs: Vec<&mut Expr>, place: Option<LocalId>, written: Option<LocalId>, after: &Live) {
        let mut counts = Vec::new();
        for e in &exprs {
            count(e, &mut counts);
        }
        if let Some(l) = place {
            counts.push((l, 2));
        }
        let moved: Live = counts
            .into_iter()
            .filter(|(l, n)| *n == 1 && !self.pinned.contains(l) && (!after.contains(l) || written == Some(*l)))
            .map(|(l, _)| l)
            .collect();
        if !moved.is_empty() {
            for e in exprs {
                rewrite(e, &moved);
            }
        }
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
            Stmt::Assign(l, path, e) => {
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
                    let mut exprs = vec![e];
                    let (place, written) = if path.is_empty() { (None, Some(*l)) } else { (Some(*l), None) };
                    for step in path.iter_mut() {
                        if let Step::Index { index, .. } = step {
                            exprs.push(index);
                        }
                    }
                    self.leaf(exprs, place, written, after);
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

    /// The locals whose reads in function `name` are moves, in the order they appear.
    fn moved(src: &str, name: &str) -> Vec<String> {
        fn expr(e: &Expr, f: &Func, out: &mut Vec<String>) {
            if let ExprKind::Move(l) = e.kind {
                out.push(f.locals[l].name.clone());
            }
            for c in children(e) {
                expr(c, f, out);
            }
        }
        fn block(stmts: &[Stmt], f: &Func, out: &mut Vec<String>) {
            for s in stmts {
                match s {
                    Stmt::Let(_, e) | Stmt::Assign(_, _, e) | Stmt::Expr(e) | Stmt::Return(Some(e)) => expr(e, f, out),
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

    #[test]
    fn a_value_named_twice_in_a_statement_stays() {
        let src = format!("{ADD}\nfn both(xs: [i64]) -> [i64] {{\n    return add(xs: xs, x: xs.len());\n}}\n");
        assert!(moved(&src, "both").is_empty());
    }
}
