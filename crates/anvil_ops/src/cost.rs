//! `anvil cost FILE`: how the work of each pure function grows. Each one is called on generated
//! arguments of growing size (arrays and text up to 40 long), its steps counted exactly; the
//! counts are fitted as constant, linear or quadratic in one parameter's size, and the tightest
//! `steps` clause that holds for every call is suggested, so a bound is measured, not guessed.

use crate::{compile, Report};
use anvil_front::ir::{Program, Ty};
use anvil_interp::Value;
use serde_json::{json, Value as Json};
use std::path::Path;

/// Calls measured per function.
const CASES: u64 = 200;

/// What one parameter's size is called in a `steps` clause, and its size in one call.
fn size_of(name: &str, ty: &Ty, v: &Value) -> Option<(String, i64)> {
    let n = match (ty, v) {
        (Ty::Str, Value::Str(s)) => s.len() as i64,
        (Ty::Bytes, Value::Bytes(b)) => b.len() as i64,
        (Ty::Array(_), Value::Array(a)) => a.len() as i64,
        (Ty::Map(..), Value::Map(m)) => m.len() as i64,
        (Ty::I64, Value::I64(i)) => return Some((name.to_string(), *i)),
        _ => return None,
    };
    Some((format!("{name}.len()"), n))
}

/// One function's measured calls: each parameter's size, and the steps the call took.
#[derive(Clone)]
pub(crate) struct Measured {
    name: String,
    signature_params: Vec<String>,
    sizes: Vec<Vec<(String, i64)>>,
    steps: Vec<u64>,
    bounds: Vec<Option<u64>>,
    bound_text: Option<String>,
}

/// The tightest `a * x + b` (a, b >= 0) above every point, by total slack, and that slack.
fn fit(points: &[(u64, u64)]) -> (u64, u64, u64) {
    // Only the most steps at each x matter.
    let mut top: Vec<(u64, u64)> = Vec::new();
    for &(x, s) in points {
        match top.iter_mut().find(|(tx, _)| *tx == x) {
            Some(t) => t.1 = t.1.max(s),
            None => top.push((x, s)),
        }
    }
    let mut candidates = vec![0u64];
    for (i, &(xi, si)) in top.iter().enumerate() {
        for &(xj, sj) in &top[i + 1..] {
            if xi != xj {
                let (dx, ds) = if xi > xj { (xi - xj, si as i128 - sj as i128) } else { (xj - xi, sj as i128 - si as i128) };
                if ds > 0 {
                    candidates.push(((ds + dx as i128 - 1) / dx as i128) as u64);
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    let mut best = (0, 0, u64::MAX);
    for a in candidates {
        // Sizes reach i64::MAX (and their squares further), so everything saturates.
        let b = top
            .iter()
            .map(|&(x, s)| (s as i128).saturating_sub((a as i128).saturating_mul(x as i128)))
            .max()
            .unwrap_or(0)
            .clamp(0, u64::MAX as i128) as u64;
        let slack: u64 = top
            .iter()
            .map(|&(x, s)| a.saturating_mul(x).saturating_add(b).saturating_sub(s))
            .fold(0u64, u64::saturating_add);
        if slack < best.2 {
            best = (a, b, slack);
        }
    }
    best
}

/// A fitted bound: its form, the parameter size it grows with, `a` and `b`, and its slack.
struct Model {
    form: &'static str,
    term: String,
    a: u64,
    b: u64,
    slack: u64,
}

impl Model {
    /// The `steps` clause, written as Anvil.
    fn clause(&self) -> String {
        let x = match self.form {
            "constant" => return format!("steps {}", self.b),
            "linear" => self.term.clone(),
            _ => format!("{t} * {t}", t = self.term),
        };
        let scaled = if self.a == 1 { x } else { format!("{} * {x}", self.a) };
        if self.b == 0 {
            format!("steps {scaled}")
        } else {
            format!("steps {scaled} + {}", self.b)
        }
    }
}

fn model(m: &Measured) -> Model {
    let most = m.steps.iter().copied().max().unwrap_or(0);
    let constant_slack: u64 = m.steps.iter().map(|s| most - s).fold(0, u64::saturating_add);
    let mut models = vec![Model { form: "constant", term: String::new(), a: 0, b: most, slack: constant_slack }];
    let params = m.sizes.first().map_or(0, Vec::len);
    for k in 0..params {
        let term = &m.sizes[0][k].0;
        // A size that is ever negative is bounded by its absolute value.
        let negative = m.sizes.iter().any(|s| s[k].1 < 0);
        let term = if negative { format!("abs({term})") } else { term.clone() };
        let xs: Vec<u64> = m.sizes.iter().map(|s| s[k].1.unsigned_abs()).collect();
        for (form, square) in [("linear", false), ("quadratic", true)] {
            let points: Vec<(u64, u64)> = xs
                .iter()
                .zip(&m.steps)
                .map(|(&x, &s)| (if square { x.saturating_mul(x) } else { x }, s))
                .collect();
            let (a, b, slack) = fit(&points);
            if a > 0 {
                models.push(Model { form, term: term.clone(), a, b, slack });
            }
        }
    }
    // The simplest form whose fit is about as tight as the best one.
    let best = models.iter().map(|m| m.slack).min().unwrap_or(0);
    let order = |f: &str| ["constant", "linear", "quadratic"].iter().position(|g| *g == f).unwrap();
    models.sort_by_key(|m| (order(m.form), m.slack));
    models
        .into_iter()
        .find(|m| m.slack <= best.saturating_add(best / 20).saturating_add(m.steps_tolerance()))
        .expect("the constant model exists")
}

impl Model {
    /// A fit within a step or two per call of the best is as good.
    fn steps_tolerance(&self) -> u64 {
        2 * CASES
    }

    /// `constant`, or `linear in xs.len()`.
    fn growth(&self) -> String {
        match self.form {
            "constant" => "constant".to_string(),
            form => format!("{form} in {}", self.term),
        }
    }

    fn order(&self) -> usize {
        ["constant", "linear", "quadratic"].iter().position(|g| *g == self.form).unwrap()
    }
}

/// How loose a fit is: its slack as a share of all the steps measured, in percent.
fn slack_percent(fitted: &Model, steps: &[u64]) -> u64 {
    let total: u64 = steps.iter().fold(0, |a, s| a.saturating_add(*s));
    if total == 0 {
        0
    } else {
        (fitted.slack as u128 * 100 / total as u128).min(u64::MAX as u128) as u64
    }
}

/// For a function with a steps bound: when its steps grow faster than the bound does, both
/// fitted over the same calls, the two growths. A bound that grows slower holds for the sizes
/// measured and breaks for larger ones. Loose fits say nothing.
pub(crate) fn outgrows(m: &Measured) -> Option<(String, String)> {
    if m.steps.is_empty() || m.bounds.iter().any(Option::is_none) {
        return None;
    }
    let steps = model(m);
    if slack_percent(&steps, &m.steps) > 50 {
        return None;
    }
    let bound_values: Vec<u64> = m.bounds.iter().map(|b| b.unwrap()).collect();
    let as_steps = Measured { steps: bound_values, ..m.clone() };
    let bound = model(&as_steps);
    (bound.order() < steps.order()).then(|| (steps.growth(), bound.growth()))
}

pub fn cost(root: &Path, file: &str, only: Option<&str>) -> Report {
    let (_, program) = match compile(root, file) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let only: Option<Vec<String>> = only.map(|o| o.split(',').map(|s| s.trim().to_string()).collect());
    let mut ffi = match crate::c_functions(&program) {
        Ok(f) => f,
        Err(e) => return Report::error(e),
    };
    let root2 = root.to_path_buf();
    let only2 = only.clone();
    let (measured, skipped) = anvil_interp::run_on_big_stack(move || measure_all(&program, only2.as_deref(), &mut ffi, &root2));
    if let Some(names) = &only {
        let known: Vec<&str> = measured.iter().map(|m| m.name.as_str()).chain(skipped.iter().map(|(n, _)| n.as_str())).collect();
        if let Some(missing) = names.iter().find(|n| !known.contains(&n.as_str())) {
            return Report::usage(format!("{file} has no function `{missing}` to measure"));
        }
    }
    let mut text = String::new();
    let mut out = Vec::new();
    for m in &measured {
        if m.steps.is_empty() {
            text.push_str(&format!("{}: no generated call returned, so nothing was measured\n", m.name));
            out.push(json!({ "function": m.name, "measured": 0 }));
            continue;
        }
        let fitted = model(m);
        let lo = m.steps.iter().min().unwrap();
        let hi = m.steps.iter().max().unwrap();
        let growth = fitted.growth();
        let slack_pct = slack_percent(&fitted, &m.steps);
        let outgrown = outgrows(m);
        text.push_str(&format!(
            "{}({}): {growth}; {} calls measured, {lo}..{hi} steps\n",
            m.name,
            m.signature_params.join(", "),
            m.steps.len()
        ));
        text.push_str(&format!("  suggested: {} (holds for every call measured)\n", fitted.clause()));
        if slack_pct > 50 {
            text.push_str(if fitted.form == "quadratic" {
                "  the closest fit is loose: it may grow faster than quadratic\n"
            } else {
                "  the closest fit is loose: its steps depend on more than one size, or on values rather than sizes\n"
            });
        }
        // How close the bound comes: the most any call used of it (small inputs often meet its
        // constant exactly), and what the costliest call used.
        let share = |s: u64, b: u64| (s as u128 * 100 / b.max(1) as u128) as u64;
        let used = m.steps.iter().zip(&m.bounds).filter_map(|(s, b)| b.map(|b| share(*s, b))).max();
        let costliest = m.steps.iter().zip(&m.bounds).max_by_key(|(s, _)| **s).and_then(|(s, b)| b.map(|b| (*s, b)));
        if let Some((steps, bound)) = &outgrown {
            text.push_str(&format!(
                "  its steps are {steps}, but its bound is {bound}: larger inputs will break it\n"
            ));
        }
        if let (Some(text_b), Some(pct), Some((s, b))) = (&m.bound_text, used, costliest) {
            text.push_str(&format!(
                "  its bound `{text_b}` holds; the most any call used is {pct}% of it, and the costliest ({s} steps) {}% of {b}\n",
                share(s, b)
            ));
        }
        // Steps by the size the fit uses (or the first parameter's): the most at each size.
        let k = m.sizes[0].iter().position(|(t, _)| fitted.term.contains(t.as_str())).unwrap_or(0);
        let mut by_size: Vec<(i64, u64)> = Vec::new();
        if !m.sizes[0].is_empty() {
            for (sz, s) in m.sizes.iter().zip(&m.steps) {
                let x = sz[k].1;
                match by_size.iter_mut().find(|(t, _)| *t == x) {
                    Some(e) => e.1 = e.1.max(*s),
                    None => by_size.push((x, *s)),
                }
            }
            by_size.sort();
            let shown: Vec<String> = pick(&by_size, 8).iter().map(|(x, s)| format!("{x}: {s}")).collect();
            text.push_str(&format!("  steps by {}: {}\n", m.sizes[0][k].0, shown.join(", ")));
        }
        out.push(json!({
            "function": m.name, "measured": m.steps.len(), "growth": fitted.form,
            "size": if fitted.form == "constant" { Json::Null } else { json!(fitted.term) },
            "suggested": fitted.clause(), "slack_percent": slack_pct, "loose": slack_pct > 50, "least": lo, "most": hi,
            "bound": m.bound_text, "bound_used_percent": used,
            "costliest_used_percent": costliest.map(|(s, b)| share(s, b)),
            "outgrows_bound": outgrown.is_some(),
            "by_size": by_size.iter().map(|(x, s)| json!([x, s])).collect::<Vec<_>>(),
        }));
    }
    for (name, why) in &skipped {
        text.push_str(&format!("{name}: not measured: {why}\n"));
        out.push(json!({ "function": name, "skipped": why }));
    }
    Report::ok(json!({ "ok": true, "file": file, "functions": out }), text)
}

/// About `n` entries spread evenly over `xs`, the first and last included.
fn pick<T: Clone>(xs: &[T], n: usize) -> Vec<T> {
    if xs.len() <= n {
        return xs.to_vec();
    }
    (0..n).map(|i| xs[i * (xs.len() - 1) / (n - 1)].clone()).collect()
}

/// Measures every pure function of the root module (or those in `only`) on the interpreter's
/// thread, where its values live; the others are listed with why they were not.
fn measure_all(
    program: &Program,
    only: Option<&[String]>,
    ffi: &mut Option<crate::Ffi>,
    root: &Path,
) -> (Vec<Measured>, Vec<(String, String)>) {
    let mut measured = Vec::new();
    let mut skipped = Vec::new();
    for (id, f) in program.funcs.iter().enumerate() {
        if f.module != 0 || f.captures.is_some() || f.external.is_some() || program.main == Some(id) {
            continue;
        }
        if only.is_some_and(|o| !o.contains(&f.name)) {
            continue;
        }
        let why = if !f.effects.is_pure() {
            Some("it has effects")
        } else if f.params.is_empty() {
            Some("it has no parameters, so nothing to grow")
        } else if !f.params.iter().all(|p| anvil_interp::generatable(&f.locals[*p].ty, program)) {
            Some("a parameter is a function")
        } else {
            None
        };
        if let Some(why) = why {
            skipped.push((f.name.clone(), why.to_string()));
            continue;
        }
        measured.push(measure_one(program, id, ffi, root));
    }
    (measured, skipped)
}

/// Measures pure function `id` (whose parameters can all be generated) on CASES calls.
pub(crate) fn measure_one(program: &Program, id: usize, ffi: &mut Option<crate::Ffi>, root: &Path) -> Measured {
    let f = &program.funcs[id];
    let mut m = Measured {
        name: f.name.clone(),
        signature_params: f.params.iter().map(|p| f.locals[*p].name.clone()).collect(),
        sizes: Vec::new(),
        steps: Vec::new(),
        bounds: Vec::new(),
        bound_text: f.steps.as_ref().map(|c| format!("steps {}", c.message.rsplit(": ").next().unwrap_or(""))),
    };
    let mut err = Vec::new();
    let host = crate::checked_host(ffi, root, &[], None, &mut err, &[]);
    let limit = Some(crate::testing::CONTRACT_STEPS);
    anvil_interp::measure(program, id, CASES, limit, host, |inputs, steps, bound| {
        let sizes: Vec<(String, i64)> = f
            .params
            .iter()
            .zip(inputs)
            .filter_map(|(p, v)| size_of(&f.locals[*p].name, &f.locals[*p].ty, v))
            .collect();
        m.sizes.push(sizes);
        m.steps.push(steps);
        m.bounds.push(bound);
    });
    m
}
