//! `anvil fuzz`: random well-typed programs, each run in the interpreter and as a native
//! executable (with the leak check); any difference in output, panic or exit code is a bug in
//! one of the backends. Programs come from a seed, so `anvil fuzz --seed S --count 1` remakes
//! program S exactly. Every program ends: helpers only call earlier helpers, and loops count to
//! small constants. Panics (overflow, a bad index, division by zero) are fine; both sides must
//! give the same one.

use crate::{golden, unique, Report, EXIT_FAILED, EXIT_OK};
use anvil_front::span::SourceFile;
use anvil_interp::gen::Rng;
use serde_json::json;
use std::path::Path;

#[derive(Clone, Copy, PartialEq)]
enum T {
    I,
    F,
    B,
    S,
    /// [i64]
    Ai,
    /// [str]
    As,
}

impl T {
    fn name(self) -> &'static str {
        match self {
            T::I => "i64",
            T::F => "f64",
            T::B => "bool",
            T::S => "str",
            T::Ai => "[i64]",
            T::As => "[str]",
        }
    }
    fn printable(self) -> bool {
        matches!(self, T::I | T::F | T::B | T::S)
    }
}

const ALL: [T; 6] = [T::I, T::F, T::B, T::S, T::Ai, T::As];
const WORDS: &[&str] = &["", "a", "abc", "x,y", "a,,b", "héllo", "漢字", "1,2,3"];

struct Sig {
    name: String,
    params: Vec<(String, T)>,
    ret: T,
}

struct Gen {
    rng: Rng,
    /// Variables in scope: (name, type).
    vars: Vec<(String, T)>,
    fns: Vec<Sig>,
    /// Function values in scope: (name, parameter type, result type). Literals among them
    /// capture the variables their bodies read.
    closures: Vec<(String, T, T)>,
    next: usize,
    /// Whether the function being written may draw random numbers (main may; helpers are pure),
    /// and whether it has.
    random: bool,
    used_random: bool,
}

impl Gen {
    fn pick<'x, X>(&mut self, xs: &'x [X]) -> &'x X {
        &xs[self.rng.below(xs.len() as u64) as usize]
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("_{prefix}{}", self.next)
    }

    fn var_of(&mut self, t: T) -> Option<String> {
        let vs: Vec<String> = self.vars.iter().filter(|(_, vt)| *vt == t).map(|(n, _)| n.clone()).collect();
        if vs.is_empty() {
            None
        } else {
            Some(self.pick(&vs).clone())
        }
    }

    fn int_lit(&mut self) -> String {
        match self.rng.below(20) {
            0 => "9223372036854775807".into(),
            1 => "(-9223372036854775807 - 1)".into(),
            2 | 3 => "0".into(),
            _ => {
                let v = self.rng.below(41) as i64 - 20;
                if v < 0 {
                    format!("({v})")
                } else {
                    v.to_string()
                }
            }
        }
    }

    fn float_lit(&mut self) -> String {
        match self.rng.below(8) {
            0 => "1.0e300".into(),
            1 => "0.1".into(),
            2 => "(-0.0)".into(),
            _ => {
                let v = (self.rng.below(81) as i64 - 40) as f64 / 4.0;
                let t = anvil_front::ir::format_f64(v);
                if v < 0.0 || t.starts_with('-') {
                    format!("({t})")
                } else {
                    t
                }
            }
        }
    }

    fn str_lit(&mut self) -> String {
        format!("\"{}\"", self.pick(WORDS))
    }

    /// An expression of type `t`; `d` bounds its depth.
    fn expr(&mut self, t: T, d: u32) -> String {
        if self.random && matches!(t, T::I | T::F) && self.rng.below(12) == 0 {
            self.used_random = true;
            return match t {
                // Mostly a bound that works; sometimes any value, which may panic.
                T::I if self.rng.below(4) == 0 => format!("random_below({})", self.expr(T::I, d.saturating_sub(1))),
                T::I => format!("random_below({})", 1 + self.rng.below(1000)),
                _ => "random_f64()".into(),
            };
        }
        if d == 0 || self.rng.below(4) == 0 {
            return self.leaf(t);
        }
        let d = d - 1;
        match t {
            T::I => match self.rng.below(12) {
                9 => {
                    let op = *self.pick(&["&", "|"]);
                    format!("({} {op} {})", self.expr(T::I, d), self.expr(T::I, d))
                }
                10 => {
                    // Mostly an amount that works; sometimes any value, which may panic.
                    let n = if self.rng.below(4) == 0 { self.expr(T::I, d) } else { self.rng.below(64).to_string() };
                    match self.rng.below(3) {
                        0 => format!("({} << {n})", self.atom(T::I, d)),
                        1 => format!("({} >> {n})", self.atom(T::I, d)),
                        _ => format!("{}.unsigned_shr({n})", self.atom(T::I, d)),
                    }
                }
                11 => {
                    let m = *self.pick(&["xor", "wrapping_add", "wrapping_sub", "wrapping_mul"]);
                    match self.rng.below(4) {
                        0 => format!("(~{})", self.atom(T::I, d)),
                        _ => format!("{}.{m}({})", self.atom(T::I, d), self.expr(T::I, d)),
                    }
                }
                0..=2 => {
                    let op = *self.pick(&["+", "-", "*", "/", "%"]);
                    format!("({} {op} {})", self.expr(T::I, d), self.expr(T::I, d))
                }
                3 => format!("abs({})", self.expr(T::I, d)),
                4 => {
                    let which = *self.pick(&[T::S, T::Ai, T::As]);
                    format!("{}.len()", self.atom(which, d))
                }
                5 => format!("to_i64({})", self.expr(T::F, d)),
                6 => {
                    // Mostly in bounds, so the rest of the program runs; sometimes anything.
                    let index = if self.rng.below(4) == 0 { self.expr(T::I, d) } else { self.rng.below(3).to_string() };
                    format!("{}[{index}]", self.atom(T::Ai, d))
                }
                7 => self.call(T::I, d).unwrap_or_else(|| self.leaf(T::I)),
                _ => format!("(-{})", self.atom(T::I, d)),
            },
            T::F => match self.rng.below(6) {
                0..=2 => {
                    let op = *self.pick(&["+", "-", "*", "/"]);
                    format!("({} {op} {})", self.expr(T::F, d), self.expr(T::F, d))
                }
                3 => format!("to_f64({})", self.expr(T::I, d)),
                4 => format!("sqrt(abs({}))", self.expr(T::F, d)),
                _ => self.call(T::F, d).unwrap_or_else(|| self.leaf(T::F)),
            },
            T::B => match self.rng.below(8) {
                0 => {
                    let op = *self.pick(&["<", "<=", "==", "!=", ">", ">="]);
                    format!("({} {op} {})", self.expr(T::I, d), self.expr(T::I, d))
                }
                1 => {
                    let op = *self.pick(&["<", "==", ">="]);
                    format!("({} {op} {})", self.expr(T::F, d), self.expr(T::F, d))
                }
                2 => {
                    let op = *self.pick(&["<", "==", "!="]);
                    format!("({} {op} {})", self.expr(T::S, d), self.expr(T::S, d))
                }
                3 => format!("({} == {})", self.atom(T::Ai, d), self.atom(T::Ai, d)),
                4 => {
                    let op = *self.pick(&["&&", "||"]);
                    format!("({} {op} {})", self.expr(T::B, d), self.expr(T::B, d))
                }
                5 => format!("!{}", self.atom(T::B, d)),
                6 => {
                    let m = *self.pick(&["contains", "starts_with", "ends_with"]);
                    format!("{}.{m}({})", self.atom(T::S, d), self.atom(T::S, d))
                }
                _ => self.call(T::B, d).unwrap_or_else(|| self.leaf(T::B)),
            },
            T::S => match self.rng.below(6) {
                0 | 1 => format!("({} + {})", self.expr(T::S, d), self.expr(T::S, d)),
                2 => {
                    // Interpolation of names only, so the braces hold no braces.
                    let parts: Vec<String> = (0..2)
                        .map(|_| {
                            let t = *self.pick(&[T::I, T::F, T::B, T::S]);
                            self.var_of(t).map(|v| format!("{{{v}}}")).unwrap_or_else(|| "-".into())
                        })
                        .collect();
                    format!("\"<{}>\"", parts.join("|"))
                }
                3 => {
                    let (a, b) = (self.rng.below(4), self.rng.below(6));
                    format!("{}[{a}..{b}]", self.atom(T::S, d))
                }
                _ => self.call(T::S, d).unwrap_or_else(|| self.leaf(T::S)),
            },
            T::Ai => match self.rng.below(4) {
                0 => {
                    let n = self.rng.below(4);
                    let items: Vec<String> = (0..n).map(|_| self.expr(T::I, d)).collect();
                    if items.is_empty() {
                        self.leaf(T::Ai)
                    } else {
                        format!("[{}]", items.join(", "))
                    }
                }
                1 => {
                    let (a, b) = (self.rng.below(3), self.rng.below(4));
                    format!("{}[{a}..{b}]", self.atom(T::Ai, d))
                }
                2 => self.parallel(T::I, d).unwrap_or_else(|| self.leaf(T::Ai)),
                _ => self.call(T::Ai, d).unwrap_or_else(|| self.leaf(T::Ai)),
            },
            T::As => match self.rng.below(3) {
                0 => {
                    let sep = *self.pick(&[",", "a", "é"]);
                    format!("{}.split(\"{sep}\")", self.atom(T::S, d))
                }
                1 => self.parallel(T::S, d).unwrap_or_else(|| self.leaf(T::As)),
                _ => self.call(T::As, d).unwrap_or_else(|| self.leaf(T::As)),
            },
        }
    }

    /// A variable, a literal, or a parenthesized expression: something a postfix can follow.
    fn atom(&mut self, t: T, d: u32) -> String {
        if self.rng.below(2) == 0 {
            self.leaf(t)
        } else {
            let e = self.expr(t, d);
            if e.starts_with('(') || e.starts_with('[') || e.starts_with('"') || !e.contains(' ') {
                e
            } else {
                format!("({e})")
            }
        }
    }

    fn leaf(&mut self, t: T) -> String {
        if self.rng.below(3) > 0 {
            if let Some(v) = self.var_of(t) {
                return v;
            }
        }
        match t {
            T::I => self.int_lit(),
            T::F => self.float_lit(),
            T::B => (*self.pick(&["true", "false"])).into(),
            T::S => self.str_lit(),
            T::Ai => format!("[{}, {}]", self.int_lit(), self.int_lit()),
            T::As => format!("[{}, {}]", self.str_lit(), self.str_lit()),
        }
    }

    /// A call to an earlier helper returning `t`, with its arguments named when it has several,
    /// or to a function value in scope.
    fn call(&mut self, t: T, d: u32) -> Option<String> {
        let candidates: Vec<usize> = (0..self.fns.len()).filter(|i| self.fns[*i].ret == t).collect();
        let values: Vec<usize> = (0..self.closures.len()).filter(|i| self.closures[*i].2 == t).collect();
        if !values.is_empty() && (candidates.is_empty() || self.rng.below(2) == 0) {
            let i = *self.pick(&values);
            let (name, pt, _) = self.closures[i].clone();
            return Some(format!("{name}({})", self.expr(pt, d)));
        }
        if candidates.is_empty() {
            return None;
        }
        let f = *self.pick(&candidates);
        let params: Vec<(String, T)> = self.fns[f].params.clone();
        let name = self.fns[f].name.clone();
        let args: Vec<String> = params
            .iter()
            .map(|(p, pt)| {
                let e = self.expr(*pt, d);
                if params.len() > 1 {
                    format!("{p}: {e}")
                } else {
                    e
                }
            })
            .collect();
        Some(format!("{name}({})", args.join(", ")))
    }

    /// `xs.parallel_map(f)` with a function value in scope or a one-parameter helper that gives
    /// `elem`, over an array of its parameter's type ([i64] or [str]).
    fn parallel(&mut self, elem: T, d: u32) -> Option<String> {
        let values: Vec<(String, T)> = self
            .closures
            .iter()
            .map(|(n, pt, rt)| (n, *pt, *rt))
            .chain(self.fns.iter().filter(|f| f.params.len() == 1).map(|f| (&f.name, f.params[0].1, f.ret)))
            .filter(|(_, pt, rt)| *rt == elem && matches!(pt, T::I | T::S))
            .map(|(n, pt, _)| (n.clone(), pt))
            .collect();
        if values.is_empty() {
            return None;
        }
        let (name, pt) = self.pick(&values).clone();
        let xs = self.atom(if pt == T::I { T::Ai } else { T::As }, d);
        Some(format!("{xs}.parallel_map({name})"))
    }

    /// A few statements binding new variables, one of them maybe a counted loop.
    fn statements(&mut self, out: &mut String, indent: &str, n: u64) {
        for _ in 0..n {
            let t = *self.pick(&ALL);
            match self.rng.below(5) {
                0 => {
                    // A counted loop updating an accumulator.
                    let acc = self.fresh("v");
                    let i = self.fresh("i");
                    let k = self.rng.below(5);
                    out.push_str(&format!("{indent}var {acc}: i64 = {};\n", self.expr(T::I, 1)));
                    out.push_str(&format!("{indent}for {i} in 0..{k} {{\n"));
                    self.vars.push((i.clone(), T::I));
                    let step = self.expr(T::I, 2);
                    out.push_str(&format!("{indent}    {acc} = {acc} + {step};\n{indent}}}\n"));
                    self.vars.retain(|(n, _)| *n != i);
                    self.vars.push((acc, T::I));
                }
                1 => {
                    // A var changed under a condition.
                    let v = self.fresh("v");
                    out.push_str(&format!("{indent}var {v}: {} = {};\n", t.name(), self.expr(t, 2)));
                    let cond = self.expr(T::B, 2);
                    let other = self.expr(t, 2);
                    out.push_str(&format!("{indent}if {cond} {{\n{indent}    {v} = {other};\n{indent}}}\n"));
                    self.vars.push((v, t));
                }
                2 if self.random && t == T::I => {
                    self.used_random = true;
                    out.push_str(&format!("{indent}random_seed({});\n", self.expr(T::I, 2)));
                }
                2 if t == T::Ai => {
                    let v = self.fresh("v");
                    out.push_str(&format!("{indent}var {v}: [i64] = {};\n", self.expr(T::Ai, 1)));
                    out.push_str(&format!("{indent}{v}.push({});\n", self.expr(T::I, 2)));
                    self.vars.push((v, T::Ai));
                }
                2 if t == T::S => {
                    // Text built by appending (in place natively); sometimes a part reads the
                    // text itself, or a copy of it is kept, which must not change.
                    let v = self.fresh("v");
                    let i = self.fresh("i");
                    out.push_str(&format!("{indent}var {v}: str = {};\n", self.expr(T::S, 1)));
                    if self.rng.below(3) == 0 {
                        let kept = self.fresh("v");
                        out.push_str(&format!("{indent}let {kept}: str = {v};\n"));
                        self.vars.push((kept, T::S));
                    }
                    let k = self.rng.below(6);
                    out.push_str(&format!("{indent}// appended to in place\n{indent}for {i} in 0..{k} {{\n"));
                    self.vars.push((i.clone(), T::I));
                    if self.rng.below(4) == 0 {
                        self.vars.push((v.clone(), T::S));
                    }
                    let parts: Vec<String> = (0..1 + self.rng.below(3)).map(|_| self.expr(T::S, 2)).collect();
                    out.push_str(&format!("{indent}    {v} = {v} + {};\n{indent}}}\n", parts.join(" + ")));
                    self.vars.retain(|(n, _)| *n != i && *n != v);
                    self.vars.push((v, T::S));
                }
                3 => self.function_value(out, indent, t),
                _ => {
                    let v = self.fresh("v");
                    out.push_str(&format!("{indent}let {v}: {} = {};\n", t.name(), self.expr(t, 3)));
                    self.vars.push((v, t));
                }
            }
        }
    }

    /// `let v: fn(P) -> R = ...;`: a helper with one parameter, or a literal whose body reads
    /// (so captures) the variables in scope. Its body draws no random numbers.
    fn function_value(&mut self, out: &mut String, indent: &str, ret: T) {
        let v = self.fresh("v");
        let helpers: Vec<usize> = (0..self.fns.len()).filter(|i| self.fns[*i].params.len() == 1).collect();
        if !helpers.is_empty() && self.rng.below(3) == 0 {
            let f = *self.pick(&helpers);
            let (name, pt, rt) = (self.fns[f].name.clone(), self.fns[f].params[0].1, self.fns[f].ret);
            out.push_str(&format!("{indent}let {v}: fn({}) -> {} = {name};\n", pt.name(), rt.name()));
            self.closures.push((v, pt, rt));
            return;
        }
        let p = self.fresh("p");
        let pt = *self.pick(&ALL);
        let random = std::mem::replace(&mut self.random, false);
        self.vars.push((p.clone(), pt));
        let body = self.expr(ret, 3);
        self.vars.retain(|(n, _)| *n != p);
        self.random = random;
        let ty = format!("fn({}) -> {}", pt.name(), ret.name());
        out.push_str(&format!(
            "{indent}let {v}: {ty} = fn({p}: {}) -> {} {{\n{indent}    return {body};\n{indent}}};\n",
            pt.name(),
            ret.name()
        ));
        self.closures.push((v, pt, ret));
    }

    fn program(&mut self) -> String {
        let mut out = String::from("// anvil fuzz program\n");
        for f in 0..1 + self.rng.below(3) {
            let params: Vec<(String, T)> =
                (0..1 + self.rng.below(3)).map(|_| (self.fresh("p"), *self.pick(&ALL))).collect();
            let ret = *self.pick(&ALL);
            let name = format!("f{f}");
            let list: Vec<String> = params.iter().map(|(p, t)| format!("{p}: {}", t.name())).collect();
            out.push_str(&format!("\nfn {name}({}) -> {} {{\n", list.join(", "), ret.name()));
            self.vars = params.clone();
            self.closures.clear();
            let n = self.rng.below(3);
            self.statements(&mut out, "    ", n);
            let value = self.expr(ret, 3);
            out.push_str(&format!("    return {value};\n}}\n"));
            self.fns.push(Sig { name, params, ret });
        }
        self.vars.clear();
        self.closures.clear();
        self.random = true;
        let mut body = String::new();
        let n = 3 + self.rng.below(5);
        self.statements(&mut body, "    ", n);
        // Every function value is called once more, and what it gives is printed below.
        // Some also go through parallel_map, on an array of arguments, printed at once.
        for (f, pt, rt) in self.closures.clone() {
            let x = self.fresh("x");
            let arg = self.leaf(pt);
            body.push_str(&format!("    let {x}: {} = {f}({arg});\n", rt.name()));
            self.vars.push((x, rt));
            if matches!(pt, T::I | T::S) && self.rng.below(2) == 0 {
                let (ys, y) = (self.fresh("x"), self.fresh("x"));
                let xs = self.leaf(if pt == T::I { T::Ai } else { T::As });
                let each = if rt.printable() { format!("{{{y}}}") } else { format!("{{{y}.len()}}") };
                body.push_str(&format!(
                    "    let {ys}: [{}] = {xs}.parallel_map({f});\n    for {y} in {ys} {{\n        print(\"{each}\");\n    }}\n",
                    rt.name()
                ));
            }
        }
        let uses = if self.used_random { "io, random" } else { "io" };
        out.push_str(&format!("\nfn main() uses {uses} {{\n{body}"));
        for (v, t) in self.vars.clone() {
            if t.printable() {
                out.push_str(&format!("    print(\"{{{v}}}\");\n"));
            } else {
                let x = self.fresh("x");
                out.push_str(&format!(
                    "    print(\"{{{v}.len()}}\");\n    for {x} in {v} {{\n        print(\"{{{x}}}\");\n    }}\n"
                ));
            }
        }
        out.push_str("}\n");
        out
    }
}

/// The program fuzz seed `seed` makes, in canonical layout.
pub fn program_for(seed: u64) -> String {
    let mut g = Gen {
        rng: Rng::new(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) ^ 0x5eed),
        vars: Vec::new(),
        fns: Vec::new(),
        closures: Vec::new(),
        next: 0,
        random: false,
        used_random: false,
    };
    let text = g.program();
    match anvil_front::fmt::format(&SourceFile::new("fuzz.anv", text.clone())) {
        Ok(t) => t,
        Err(_) => text,
    }
}

pub fn fuzz(root: &Path, seed: Option<u64>, count: Option<u64>) -> Report {
    let first = seed.unwrap_or(1);
    let count = count.unwrap_or(10);
    if count == 0 {
        return Report::usage("--count must be at least 1");
    }
    let out_dir = root.join("out").join("fuzz");
    let mut results = Vec::new();
    let mut text = String::new();
    let mut bad = 0;
    for s in first..first + count {
        let program = program_for(s);
        let (status, detail) = check_one(&program, s);
        if status != "same" {
            bad += 1;
            let file = out_dir.join(format!("{s}.anv"));
            let _ = std::fs::create_dir_all(&out_dir);
            let _ = std::fs::write(&file, &program);
            text.push_str(&format!("{} seed {s}: {detail} (program in out/fuzz/{s}.anv)\n", status.to_uppercase()));
            results.push(json!({ "seed": s, "status": status, "detail": detail, "file": format!("out/fuzz/{s}.anv") }));
        } else {
            results.push(json!({ "seed": s, "status": status }));
        }
    }
    text.push_str(&format!(
        "{} of {count} program(s) agree on both backends (seeds {first}..{})\n",
        count - bad,
        first + count - 1
    ));
    let json = json!({ "ok": bad == 0, "seed": first, "count": count, "failed": bad, "programs": results });
    Report {
        json,
        stdout: text,
        stderr: String::new(),
        exit: if bad == 0 { EXIT_OK } else { EXIT_FAILED },
        failed: bad > 0,
    }
}

/// "same", "differs", "invalid" (the generator made a program that does not check: a fuzzer
/// bug), "stopped" or "error" (native build or runtime error), and what happened.
fn check_one(text: &str, seed: u64) -> (&'static str, String) {
    let src = SourceFile::new(format!("fuzz/{seed}.anv"), text.to_string());
    let program = match anvil_front::compile(&src) {
        Ok(p) => p,
        Err(d) => return ("invalid", format!("{} error(s), first: {}", d.len(), d[0].message)),
    };
    let native_program = program.clone();
    // Fuzzed programs read no files; they run in the scratch directory all the same.
    let scratch = std::env::temp_dir().join(format!("anvil_fuzz_{}_{}", std::process::id(), unique()));
    let none = golden::Input::default();
    let interp = match golden::interpret(program, Some(1_000_000), &std::env::temp_dir(), &none) {
        Ok((o, _)) => o,
        Err(m) => return ("stopped", m),
    };
    let native = golden::native(&native_program, &scratch, &format!("fuzz{seed}"), &std::env::temp_dir(), &none);
    let _ = std::fs::remove_dir_all(&scratch);
    match native {
        Err(e) => ("error", format!("native: {e}")),
        Ok(n) => match golden::difference(&interp, &n) {
            None => ("same", String::new()),
            Some(d) => ("differs", format!("native {d}, against the interpreter")),
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn some_programs_use_bit_operations() {
        let n = (1..=50)
            .filter(|s| {
                ["<<", ">>", ".xor(", "unsigned_shr", "wrapping_"].iter().any(|o| super::program_for(*s).contains(o))
            })
            .count();
        assert!(n >= 10, "only {n} of 50 fuzz programs use bit operations");
    }

    #[test]
    fn some_programs_make_function_values() {
        let n = (1..=50).filter(|s| super::program_for(*s).contains("= fn(")).count();
        assert!(n >= 10, "only {n} of 50 fuzz programs make function literals");
    }

    #[test]
    fn some_programs_draw_random_numbers() {
        let n = (1..=50).filter(|s| super::program_for(*s).contains("random_")).count();
        assert!(n >= 5, "only {n} of 50 fuzz programs draw random numbers");
    }

    #[test]
    fn some_programs_append_text() {
        let n = (1..=50).filter(|s| super::program_for(*s).contains("// appended to in place")).count();
        assert!(n >= 5, "only {n} of 50 fuzz programs append text");
    }

    #[test]
    fn some_programs_work_in_parallel() {
        let n = (1..=50).filter(|s| super::program_for(*s).contains(".parallel_map(")).count();
        assert!(n >= 5, "only {n} of 50 fuzz programs use parallel_map");
    }
}
