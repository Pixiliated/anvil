//! What numbers an integer can be, so that a release build can leave out overflow checks that could
//! never fail.
//!
//! Every `i64` local gets a range holding each value any of its assignments can give it (`of_locals`).
//! `Known::ranges` narrows that at one point of a function, from a comparison just passed or an index
//! just checked. A sum whose operands' ranges cannot take it past an `i64` needs no check. Ranges only
//! ever say less than is true: an expression not understood can be anything.

use anvil_front::ir::*;
use std::collections::{HashMap, HashSet};

/// The lowest and highest a number can be, in a type that holds any sum or product of two `i64`s.
pub(crate) type Range = (i128, i128);

pub(crate) const ANY: Range = (i64::MIN as i128, i64::MAX as i128);

/// No array or text is longer than this: there is not the address space for it.
pub(crate) const LONGEST: i128 = 1 << 57;

pub(crate) fn narrow(a: Range, b: Range) -> Range {
    (a.0.max(b.0), a.1.min(b.1))
}

pub(crate) fn either(a: Range, b: Range) -> Range {
    (a.0.min(b.0), a.1.max(b.1))
}

/// What `a op b` can be before anything checks whether that fits.
pub(crate) fn arith(op: IOp, a: Range, b: Range) -> Range {
    match op {
        IOp::Add => (a.0 + b.0, a.1 + b.1),
        IOp::Sub => (a.0 - b.1, a.1 - b.0),
        IOp::Mul => {
            let corners = [a.0 * b.0, a.0 * b.1, a.1 * b.0, a.1 * b.1];
            (*corners.iter().min().unwrap(), *corners.iter().max().unwrap())
        }
        IOp::Div | IOp::Rem => unreachable!("division has no overflow to leave out but one"),
    }
}

pub(crate) fn fits(r: Range) -> bool {
    r.0 >= ANY.0 && r.1 <= ANY.1
}

/// The smallest number of the form 2^n - 1 that is not below `x`, which is not negative.
fn ones(x: i128) -> i128 {
    let mut m = 0;
    while m < x {
        m = m * 2 + 1;
    }
    m
}

/// What `e` can be, given what `local` says of each local. A checked operation that would not fit stops
/// the program, so any value it does give is an `i64`.
pub(crate) fn eval(e: &Expr, local: &dyn Fn(LocalId) -> Range) -> Range {
    let of = |x: &Expr| eval(x, local);
    match &e.kind {
        ExprKind::Int(v) => (*v as i128, *v as i128),
        ExprKind::Local(l) | ExprKind::Move(l) => local(*l),
        ExprKind::Builtin(Builtin::Len, _) => (0, LONGEST),
        ExprKind::Builtin(Builtin::BitLen | Builtin::LeadingZeros | Builtin::TrailingZeros, _) => (0, 64),
        ExprKind::Index { base, .. } if base.ty == Ty::Bytes => (0, 255),
        ExprKind::IArith { op: op @ (IOp::Add | IOp::Sub | IOp::Mul), l, r, .. } => {
            narrow(arith(*op, of(l), of(r)), ANY)
        }
        ExprKind::IArith { op: IOp::Div, l, r, .. } if of(r).0 >= 1 => {
            let a = of(l);
            (a.0.min(0), a.1.max(0))
        }
        ExprKind::IArith { op: IOp::Rem, l, r, .. } if of(r).0 >= 1 => {
            let (a, most) = (of(l), of(r).1 - 1);
            if a.0 < 0 {
                (-most, most)
            } else {
                (0, most.min(a.1))
            }
        }
        ExprKind::Bits { op, l, r, .. } => {
            let (a, b) = (of(l), of(r));
            match op {
                BitOp::And if a.0 >= 0 && b.0 >= 0 => (0, a.1.min(b.1)),
                BitOp::And if a.0 >= 0 => (0, a.1),
                BitOp::And if b.0 >= 0 => (0, b.1),
                BitOp::Or | BitOp::Xor if a.0 >= 0 && b.0 >= 0 => (0, ones(a.1.max(b.1))),
                BitOp::Shr | BitOp::UShr if a.0 >= 0 => (0, a.1),
                BitOp::UShr if b.0 >= 1 => (0, (1 << (64 - b.0.min(63))) - 1),
                _ => ANY,
            }
        }
        ExprKind::Cond { then, otherwise, .. } => either(of(then), of(otherwise)),
        _ => ANY,
    }
}

enum Def<'e> {
    Value(&'e Expr),
    /// The counter of `for l in start..end`.
    Counter(&'e Expr, &'e Expr),
}

#[derive(Default)]
struct Found<'e> {
    defs: Vec<(LocalId, Def<'e>)>,
    /// Given a value some other way, so it can be anything.
    opaque: HashSet<LocalId>,
}

fn exprs<'e>(e: &'e Expr, found: &mut Found<'e>) {
    if let ExprKind::Try { held, .. } = &e.kind {
        found.opaque.insert(*held);
    }
    for c in children(e) {
        exprs(c, found);
    }
}

fn block<'e>(b: &'e [Stmt], found: &mut Found<'e>) {
    for s in b {
        match s {
            Stmt::Let(l, e) => {
                found.defs.push((*l, Def::Value(e)));
                exprs(e, found);
            }
            Stmt::Assign(l, path, e, _) => {
                if path.is_empty() {
                    found.defs.push((*l, Def::Value(e)));
                }
                for step in path {
                    if let Step::Index { index, .. } = step {
                        exprs(index, found);
                    }
                }
                exprs(e, found);
            }
            Stmt::Expr(e) | Stmt::Return(Some(e)) => exprs(e, found),
            Stmt::If(c, t, f) => {
                exprs(c, found);
                block(t, found);
                block(f, found);
            }
            Stmt::While(c, body) => {
                exprs(c, found);
                block(body, found);
            }
            Stmt::For(l, start, end, body) => {
                found.defs.push((*l, Def::Counter(start, end)));
                exprs(start, found);
                exprs(end, found);
                block(body, found);
            }
            Stmt::ForEach { var, held, iter, body } => {
                found.opaque.extend([*var, *held]);
                exprs(iter, found);
                block(body, found);
            }
            Stmt::Match { held, scrutinee, arms } => {
                found.opaque.insert(*held);
                exprs(scrutinee, found);
                for arm in arms {
                    found.opaque.extend(arm.bindings.iter().map(|(_, l)| *l));
                    block(&arm.body, found);
                }
            }
            Stmt::Drop(_) | Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Reached(_) | Stmt::Line(_) => {}
        }
    }
}

/// A range for each `i64` local of `f` that holds whatever any assignment to it gives it. Starting from
/// "anything" and applying the assignments only ever narrows, and each step is still true of every
/// run, so stopping after a few is safe: more would only say more.
pub(crate) fn of_locals(types: &[Ty], f: &Func) -> HashMap<LocalId, Range> {
    let mut found = Found::default();
    found.opaque.extend(f.params.iter().copied());
    found.opaque.extend(f.result_local);
    block(&f.body, &mut found);
    let tracked = |l: LocalId| types[l] == Ty::I64 && !found.opaque.contains(&l);
    let mut ranges: HashMap<LocalId, Range> = HashMap::new();
    for _ in 0..3 {
        let mut next: HashMap<LocalId, Range> = HashMap::new();
        let look = |l: LocalId| ranges.get(&l).copied().unwrap_or(ANY);
        for (l, def) in &found.defs {
            if !tracked(*l) {
                continue;
            }
            let r = match def {
                Def::Value(e) => eval(e, &look),
                Def::Counter(start, end) => {
                    let (first, limit) = (eval(start, &look), eval(end, &look));
                    (first.0, first.0.max(limit.1 - 1))
                }
            };
            next.entry(*l).and_modify(|x| *x = either(*x, r)).or_insert(r);
        }
        ranges = next;
    }
    ranges
}
