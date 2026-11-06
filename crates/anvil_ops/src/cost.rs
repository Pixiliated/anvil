//! `anvil cost FILE`: how the work and the memory of each pure function grow. Each one is called
//! on generated arguments of growing size (arrays and text up to 40 long), its steps and the
//! bytes it allocates counted exactly; each is fitted as constant, logarithmic, linear or quadratic
//! in one size (of a parameter, or of a field of a struct parameter), and the tightest `steps` and
//! `allocs` clauses that hold for every call are suggested, so a bound is measured, not guessed.

use crate::{compile, Report};
use anvil_front::ir::{Check, Program, Ty};
use anvil_interp::Value;
use serde_json::{json, Value as Json};
use std::path::Path;

/// Calls measured per function.
const CASES: u64 = 200;

/// The sizes a bound can name in one parameter (`xs.len()`, `n`, `h.keys.len()`), with what each
/// is in one call. Which there are depends only on the type, so every call lists the same ones.
fn sizes_of(name: &str, ty: &Ty, v: &Value, program: &Program, out: &mut Vec<(String, i64)>) {
    let n = match (ty, v) {
        (Ty::Str, Value::Str(s)) => s.len() as i64,
        (Ty::Bytes, Value::Bytes(b)) => b.len() as i64,
        (Ty::Array(_), Value::Array(a)) => a.len() as i64,
        (Ty::Map(..), Value::Map(m)) => m.len() as i64,
        (Ty::I64, Value::I64(i)) => return out.push((name.to_string(), *i)),
        (Ty::Struct(id), Value::Struct(fields)) => {
            for ((field, t), f) in program.structs[*id].fields.iter().zip(fields.iter()) {
                sizes_of(&format!("{name}.{field}"), t, f, program, out);
            }
            return;
        }
        _ => return,
    };
    out.push((format!("{name}.len()"), n));
}

/// The bits it takes to write `x`, as `bit_len` gives them.
fn bits(x: u64) -> u64 {
    (64 - x.leading_zeros()) as u64
}

/// Whether doubling a size adds to `values` rather than multiplying them: at the largest size, at
/// most half as much again as anywhere up to half that size. Slack alone cannot tell: a big enough
/// multiple of the bits rides above what doubles in steps. The extremes of generated integers say
/// nothing about growth, so they are left out.
fn doubling_adds(sizes: &[u64], values: &[u64]) -> bool {
    let points = || sizes.iter().zip(values).filter(|(x, _)| **x <= 1 << 20);
    let Some(&largest) = points().map(|(x, _)| x).max() else { return false };
    let at_largest = points().filter(|(x, _)| **x == largest).map(|(_, v)| *v).max().unwrap_or(0);
    let up_to_half = points().filter(|(x, _)| **x <= largest / 2).map(|(_, v)| *v).max().unwrap_or(0);
    largest >= 16 && up_to_half > 0 && at_largest.saturating_mul(2) <= up_to_half.saturating_mul(3)
}

/// One resource measured over a function's calls: steps or bytes allocated.
#[derive(Clone)]
pub(crate) struct Resource {
    /// The clause that bounds it (`steps`, `allocs`) and its unit (`steps`, `bytes`).
    clause: &'static str,
    unit: &'static str,
    values: Vec<u64>,
    bounds: Vec<Option<u64>>,
    /// The function's own clause, as written.
    bound_text: Option<String>,
}

/// One function's measured calls: each parameter's size, and what each call used.
#[derive(Clone)]
pub(crate) struct Measured {
    name: String,
    signature_params: Vec<String>,
    sizes: Vec<Vec<(String, i64)>>,
    resources: Vec<Resource>,
}

/// The tightest `a * x + b` (a, b >= 0) above every point, by total slack, and that slack.
fn fit(points: &[(u64, u64)]) -> (u64, u64, u64) {
    // Only the most at each x matters.
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

/// A fitted bound: its form, the parameter size it grows with, `a` and `b`, and its slack. A
/// quadratic one may be `triangular`: in x * (x + 1) / 2, what copying everything so far once
/// each round adds up to (1 + 2 + ... + x), which whole multiples of x * x overshoot twice over.
struct Model {
    form: &'static str,
    term: String,
    triangular: bool,
    a: u64,
    b: u64,
    slack: u64,
}

impl Model {
    /// The clause (`steps` or `allocs`), written as Anvil.
    fn clause(&self, keyword: &str) -> String {
        let x = match self.form {
            "constant" => return format!("{keyword} {}", self.b),
            "linear" => self.term.clone(),
            "logarithmic" => format!("{}.bit_len()", self.term),
            // x * (x + 1) is even, so the division is exact; a multiplies first.
            _ if self.triangular => {
                let t = &self.term;
                let scaled = if self.a == 1 { format!("{t} * ({t} + 1) / 2") } else { format!("{} * {t} * ({t} + 1) / 2", self.a) };
                return if self.b == 0 { format!("{keyword} {scaled}") } else { format!("{keyword} {scaled} + {}", self.b) };
            }
            _ => format!("{t} * {t}", t = self.term),
        };
        let scaled = if self.a == 1 { x } else { format!("{} * {x}", self.a) };
        if self.b == 0 {
            format!("{keyword} {scaled}")
        } else {
            format!("{keyword} {scaled} + {}", self.b)
        }
    }

    /// `constant`, or `linear in xs.len()`.
    fn growth(&self) -> String {
        match self.form {
            "constant" => "constant".to_string(),
            form => format!("{form} in {}", self.term),
        }
    }

    fn order(&self) -> usize {
        ["constant", "logarithmic", "linear", "quadratic"].iter().position(|g| *g == self.form).unwrap()
    }
}

/// The simplest of constant, logarithmic, linear or quadratic in one size that fits `values` about
/// as tightly as the best (within 5%, or two units a call; logarithmic only within 5%, since over
/// sizes this small two units a call would take in much that is linear).
fn model(sizes: &[Vec<(String, i64)>], values: &[u64]) -> Model {
    let most = values.iter().copied().max().unwrap_or(0);
    let constant_slack: u64 = values.iter().map(|s| most - s).fold(0, u64::saturating_add);
    let mut models =
        vec![Model { form: "constant", term: String::new(), triangular: false, a: 0, b: most, slack: constant_slack }];
    let params = sizes.first().map_or(0, Vec::len);
    for k in 0..params {
        let term = &sizes[0][k].0;
        // A size that is ever negative is bounded by its absolute value.
        let negative = sizes.iter().any(|s| s[k].1 < 0);
        let term = if negative { format!("abs({term})") } else { term.clone() };
        let xs: Vec<u64> = sizes.iter().map(|s| s[k].1.unsigned_abs()).collect();
        let shapes: [(&str, bool, fn(u64) -> u64); 4] = [
            ("logarithmic", false, bits),
            ("linear", false, |x| x),
            ("quadratic", false, |x| x.saturating_mul(x)),
            ("quadratic", true, |x| x.saturating_mul(x.saturating_add(1)) / 2),
        ];
        for (form, triangular, shape) in shapes {
            if form == "logarithmic" && !doubling_adds(&xs, values) {
                continue;
            }
            let points: Vec<(u64, u64)> = xs.iter().zip(values).map(|(&x, &s)| (shape(x), s)).collect();
            let (a, b, slack) = fit(&points);
            if a > 0 {
                models.push(Model { form, term: term.clone(), triangular, a, b, slack });
            }
        }
    }
    let best = models.iter().map(|m| m.slack).min().unwrap_or(0);
    let tolerance = best.saturating_add(best / 20).saturating_add(2 * CASES);
    models.sort_by_key(|m| (m.order(), m.slack));
    let close = best.saturating_add(best / 20);
    let fits = |m: &Model| m.slack <= if m.form == "logarithmic" { close } else { tolerance };
    models.into_iter().find(fits).expect("the tightest model fits")
}

/// How loose a fit is: its slack as a share of all that was measured, in percent.
fn slack_percent(fitted: &Model, values: &[u64]) -> u64 {
    let total: u64 = values.iter().fold(0, |a, s| a.saturating_add(*s));
    if total == 0 {
        0
    } else {
        (fitted.slack as u128 * 100 / total as u128).min(u64::MAX as u128) as u64
    }
}

/// For each bounded resource that grows faster than its bound does (both fitted over the same
/// calls): its unit, its growth and the bound's. Such a bound holds for the sizes measured and
/// breaks for larger ones. Loose fits say nothing.
pub(crate) fn outgrows(m: &Measured) -> Vec<(&'static str, &'static str, String, String)> {
    let mut out = Vec::new();
    for r in &m.resources {
        if r.values.is_empty() || r.bounds.iter().any(Option::is_none) {
            continue;
        }
        let used = model(&m.sizes, &r.values);
        if slack_percent(&used, &r.values) > 50 {
            continue;
        }
        let bound_values: Vec<u64> = r.bounds.iter().map(|b| b.unwrap()).collect();
        let bound = model(&m.sizes, &bound_values);
        // An i64 has 64 bits, so what grows with its bits has a most, which a constant can be above.
        let capped = used.form == "logarithmic"
            && bound.form == "constant"
            && used.a.saturating_mul(64).saturating_add(used.b) <= bound.b;
        if bound.order() < used.order() && !capped {
            out.push((r.clause, r.unit, used.growth(), bound.growth()));
        }
    }
    out
}

pub fn cost(root: &Path, file: &str, only: Option<&str>) -> Report {
    let (_, program) = match compile(root, file) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let only: Option<Vec<String>> = only.map(names_in);
    let mut ffi = match crate::c_functions(&program) {
        Ok(f) => f,
        Err(e) => return Report::error(e),
    };
    let root2 = root.to_path_buf();
    let only2 = only.clone();
    let (measured, skipped) =
        anvil_interp::run_on_big_stack(move || measure_all(&program, only2.as_deref(), &mut ffi, &root2));
    if let Some(names) = &only {
        let known: Vec<&str> =
            measured.iter().map(|m| m.name.as_str()).chain(skipped.iter().map(|(n, _)| n.as_str())).collect();
        if let Some(missing) = names.iter().find(|n| !known.iter().any(|k| named(n, k))) {
            return Report::usage(format!("{file} has no function `{missing}` to measure"));
        }
    }
    let mut text = String::new();
    let mut out = Vec::new();
    for m in &measured {
        let calls = m.resources[0].values.len();
        if calls == 0 {
            text.push_str(&format!("{}: no generated call returned, so nothing was measured\n", m.name));
            out.push(json!({ "function": m.name, "measured": 0 }));
            continue;
        }
        text.push_str(&format!("{}({}): {calls} calls measured\n", m.name, m.signature_params.join(", ")));
        let outgrown = outgrows(m);
        let mut v = json!({ "function": m.name, "measured": calls });
        for r in &m.resources {
            v[r.unit] = resource(m, r, &outgrown, &mut text);
        }
        out.push(v);
    }
    for (name, why) in &skipped {
        text.push_str(&format!("{name}: not measured: {why}\n"));
        out.push(json!({ "function": name, "skipped": why }));
    }
    Report::ok(json!({ "ok": true, "file": file, "functions": out }), text)
}

/// One resource of one function: its growth, suggested clause, how its bound holds, and the
/// most it took at each size; the text goes to `text`.
fn resource(
    m: &Measured,
    r: &Resource,
    outgrown: &[(&'static str, &'static str, String, String)],
    text: &mut String,
) -> Json {
    let fitted = model(&m.sizes, &r.values);
    let lo = r.values.iter().min().unwrap();
    let hi = r.values.iter().max().unwrap();
    let slack_pct = slack_percent(&fitted, &r.values);
    let suggested = fitted.clause(r.clause);
    text.push_str(&format!("  {}: {}, {lo}..{hi}; suggested `{suggested}`\n", r.unit, fitted.growth()));
    if slack_pct > 50 {
        text.push_str(if fitted.form == "quadratic" {
            "    the closest fit is loose: it may grow faster than quadratic\n"
        } else {
            "    the closest fit is loose: it depends on more than one size, or on values rather than sizes\n"
        });
    }
    let mine = outgrown.iter().find(|o| o.1 == r.unit);
    if let Some((_, unit, used, bound)) = mine {
        text.push_str(&format!("    its {unit} are {used}, but its bound is {bound}: larger inputs will break it\n"));
    }
    // How close the bound comes: the most any call used of it (small inputs often meet its
    // constant exactly), and what the costliest call used.
    let share = |s: u64, b: u64| (s as u128 * 100 / b.max(1) as u128) as u64;
    let used = r.values.iter().zip(&r.bounds).filter_map(|(s, b)| b.map(|b| share(*s, b))).max();
    let holds = r.values.iter().zip(&r.bounds).all(|(s, b)| b.is_none_or(|b| *s <= b));
    let costliest = r.values.iter().zip(&r.bounds).max_by_key(|(s, _)| **s).and_then(|(s, b)| b.map(|b| (*s, b)));
    if let (Some(text_b), Some(pct), Some((s, b))) = (&r.bound_text, used, costliest) {
        let holds = if holds { "holds" } else { "does not hold" };
        text.push_str(&format!(
            "    its bound `{text_b}` {holds}; the most any call used is {pct}% of it, and the costliest ({s}) {}% of {b}\n",
            share(s, b)
        ));
    }
    // The most at each size the fit uses (or the first parameter's).
    let mut by_size: Vec<(i64, u64)> = Vec::new();
    if !m.sizes[0].is_empty() {
        let k = m.sizes[0].iter().position(|(t, _)| fitted.term.contains(t.as_str())).unwrap_or(0);
        for (sz, s) in m.sizes.iter().zip(&r.values) {
            let x = sz[k].1;
            match by_size.iter_mut().find(|(t, _)| *t == x) {
                Some(e) => e.1 = e.1.max(*s),
                None => by_size.push((x, *s)),
            }
        }
        by_size.sort();
        // Constant in every size and the same at each: nothing to show.
        if fitted.form != "constant" || lo != hi {
            let shown: Vec<String> = pick(&by_size, 8).iter().map(|(x, s)| format!("{x}: {s}")).collect();
            text.push_str(&format!("    by {}: {}\n", m.sizes[0][k].0, shown.join(", ")));
        }
    }
    json!({
        "growth": fitted.form,
        "size": if fitted.form == "constant" { Json::Null } else { json!(fitted.term) },
        "suggested": suggested, "slack_percent": slack_pct, "loose": slack_pct > 50, "least": lo, "most": hi,
        "bound": r.bound_text, "bound_used_percent": used, "bound_holds": r.bound_text.as_ref().map(|_| holds),
        "costliest_used_percent": costliest.map(|(s, b)| share(s, b)),
        "outgrows_bound": mine.is_some(),
        "by_size": by_size.iter().map(|(x, s)| json!([x, s])).collect::<Vec<_>>(),
    })
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
        if only.is_some_and(|o| !o.iter().any(|n| named(n, &f.name))) {
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

/// The names in a comma-separated list; the commas of `push[i64, str]` do not part names.
fn names_in(list: &str) -> Vec<String> {
    let (mut out, mut name, mut depth) = (Vec::new(), String::new(), 0);
    for c in list.chars() {
        match c {
            ',' if depth == 0 => out.push(std::mem::take(&mut name)),
            _ => {
                depth += (c == '[') as i32 - (c == ']') as i32;
                name.push(c);
            }
        }
    }
    out.push(name);
    out.into_iter().map(|n| n.trim().to_string()).collect()
}

/// Whether `asked` names function `name`: exactly, or every instance of a generic by its own name.
/// Spaces inside an instance's brackets do not matter.
fn named(asked: &str, name: &str) -> bool {
    let tight = |s: &str| s.replace(' ', "");
    tight(asked) == tight(name) || name.split('[').next() == Some(asked)
}

/// A bound clause as written: `steps 2 * xs.len() + 3`.
fn clause_text(keyword: &str, c: &Option<Check>) -> Option<String> {
    c.as_ref().map(|c| format!("{keyword} {}", c.message.rsplit(": ").next().unwrap_or("")))
}

/// Measures pure function `id` (whose parameters can all be generated) on CASES calls.
pub(crate) fn measure_one(program: &Program, id: usize, ffi: &mut Option<crate::Ffi>, root: &Path) -> Measured {
    let f = &program.funcs[id];
    let empty = |clause, unit, bound_text| Resource { clause, unit, values: Vec::new(), bounds: Vec::new(), bound_text };
    let mut m = Measured {
        name: f.name.clone(),
        signature_params: f.params.iter().map(|p| f.locals[*p].name.clone()).collect(),
        sizes: Vec::new(),
        resources: vec![
            empty("steps", "steps", clause_text("steps", &f.steps)),
            empty("allocs", "bytes", clause_text("allocs", &f.allocs)),
        ],
    };
    let mut err = Vec::new();
    let host = crate::checked_host(ffi, root, &[], None, &mut err, &[]);
    let limit = Some(crate::testing::CONTRACT_STEPS);
    anvil_interp::measure(program, id, CASES, limit, host, |inputs, usage| {
        let mut sizes = Vec::new();
        for (p, v) in f.params.iter().zip(inputs) {
            sizes_of(&f.locals[*p].name, &f.locals[*p].ty, v, program, &mut sizes);
        }
        m.sizes.push(sizes);
        m.resources[0].values.push(usage.steps);
        m.resources[0].bounds.push(usage.steps_bound);
        m.resources[1].values.push(usage.bytes);
        m.resources[1].bounds.push(usage.bytes_bound);
    });
    m
}
