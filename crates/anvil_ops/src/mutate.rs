//! `anvil mutate FILE`: does anything notice when the code is wrong? Each mutant changes one
//! thing in one function of FILE (an operator, a literal, a call statement), and is run against
//! the tests and goldens of FILE and of every project file that imports it. A mutant nothing
//! notices "survives": a test is missing, or asserts too little. Some survivors change nothing a
//! program can see (`a < b` for `a <= b` in a min); `allow mutants "why"` before the function says
//! so, and is reported as stale when nothing in that function survives.

use crate::golden::{self, Input, Output};
use crate::project::Project;
use crate::{Files, Report, EXIT_FAILED, EXIT_OK};
use anvil_front::ast::{self, BinOp, Else, ExprKind, InterpPart, Item, StmtKind, UnOp};
use anvil_front::ir::Program;
use anvil_front::span::SourceFile;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

/// One change to the source: replace `start..end` with `text`.
struct Mutant {
    start: usize,
    end: usize,
    text: String,
    /// What was there and what it became, as the report shows them.
    from: String,
    to: String,
    function: String,
}

/// The operator each one is mutated to: the one a slip of the hand or of thought gives.
fn swap(op: BinOp) -> Option<BinOp> {
    Some(match op {
        BinOp::Add => BinOp::Sub,
        BinOp::Sub => BinOp::Add,
        BinOp::Mul => BinOp::Div,
        BinOp::Div => BinOp::Mul,
        BinOp::Rem => BinOp::Div,
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        BinOp::Lt => BinOp::Le,
        BinOp::Le => BinOp::Lt,
        BinOp::Gt => BinOp::Ge,
        BinOp::Ge => BinOp::Gt,
        BinOp::And => BinOp::Or,
        BinOp::Or => BinOp::And,
        BinOp::BitAnd => BinOp::BitOr,
        BinOp::BitOr => BinOp::BitAnd,
        BinOp::Shl => BinOp::Shr,
        BinOp::Shr => BinOp::Shl,
        BinOp::Caret => return None,
    })
}

/// Collects the mutants of the functions in a file, in source order.
struct Sites<'a> {
    text: &'a str,
    function: String,
    out: Vec<Mutant>,
}

impl Sites<'_> {
    fn add(&mut self, start: usize, end: usize, text: String, from: &str, to: &str) {
        let function = self.function.clone();
        self.out.push(Mutant { start, end, text, from: from.into(), to: to.into(), function });
    }

    fn block(&mut self, b: &ast::Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            StmtKind::Let { init, .. } => self.expr(init),
            StmtKind::Assign { target, value } => {
                self.expr(target);
                self.expr(value);
            }
            StmtKind::Expr(e) => {
                // A call made for what it does: leave it out.
                if matches!(e.kind, ExprKind::Call { .. } | ExprKind::Method { .. }) {
                    let what = self.text[s.span.start..s.span.end].trim().to_string();
                    let shown = if what.chars().count() > 40 {
                        format!("{}...", what.chars().take(37).collect::<String>())
                    } else {
                        what
                    };
                    self.add(s.span.start, s.span.end, String::new(), &shown, "(left out)");
                }
                self.expr(e);
            }
            StmtKind::If { cond, then, else_ } => {
                self.expr(cond);
                self.block(then);
                match else_.as_deref() {
                    None => {}
                    Some(Else::Block(b)) => self.block(b),
                    Some(Else::If(st)) => self.stmt(st),
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            StmtKind::For { start, end, body, .. } => {
                self.expr(start);
                self.expr(end);
                self.block(body);
            }
            StmtKind::ForEach { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            StmtKind::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee);
                for a in arms {
                    self.block(&a.body);
                }
            }
            StmtKind::Return(v) => {
                if let Some(e) = v {
                    self.expr(e);
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
        }
    }

    fn expr(&mut self, e: &ast::Expr) {
        match &e.kind {
            ExprKind::Int(v) => {
                if let Some(n) = v.checked_add(1) {
                    self.add(e.span.start, e.span.end, n.to_string(), &self.text[e.span.start..e.span.end].to_string(), &n.to_string());
                }
            }
            ExprKind::Bool(b) => {
                let (from, to) = if *b { ("true", "false") } else { ("false", "true") };
                self.add(e.span.start, e.span.end, to.into(), from, to);
            }
            ExprKind::Float(..) | ExprKind::Str(_) | ExprKind::Name(_) | ExprKind::Result => {}
            ExprKind::Field(x, _) | ExprKind::Try(x) | ExprKind::Paren(x) => self.expr(x),
            ExprKind::Call { args, .. } => {
                for a in args {
                    self.expr(&a.value);
                }
            }
            ExprKind::Method { receiver, args, .. } => {
                self.expr(receiver);
                for a in args {
                    self.expr(&a.value);
                }
            }
            ExprKind::Index(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Slice(a, b, c) => {
                self.expr(a);
                self.expr(b);
                self.expr(c);
            }
            ExprKind::Array(xs) => {
                for x in xs {
                    self.expr(x);
                }
            }
            ExprKind::MapLit(pairs) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::StructLit { fields, .. } => {
                for (_, x) in fields {
                    self.expr(x);
                }
            }
            ExprKind::Unary(op, x) => {
                // `!c`, `-n` and `~n` become `c`, `n` and `n`.
                let sign = match op {
                    UnOp::Not => "!",
                    UnOp::Neg => "-",
                    UnOp::BitNot => "~",
                };
                let inner = self.text[x.span.start..x.span.end].to_string();
                self.add(e.span.start, e.span.end, inner.clone(), &format!("{sign}{inner}"), &inner);
                self.expr(x);
            }
            ExprKind::Binary(op, a, b) => {
                self.expr(a);
                if let Some(to) = swap(*op) {
                    let between = &self.text[a.span.end..b.span.start];
                    if let Some(at) = between.find(op.text()) {
                        let start = a.span.end + at;
                        self.add(start, start + op.text().len(), to.text().into(), op.text(), to.text());
                    }
                }
                self.expr(b);
            }
            // A function literal's mutants belong to the function it is written in.
            ExprKind::Lambda(d) => self.block(&d.body),
        }
    }
}

/// The mutants of the file's functions (of those in `only`), and the functions that allow
/// mutants to survive, with the offset of their names.
fn mutants(src: &SourceFile, file: &ast::File, only: Option<&[String]>) -> (Vec<Mutant>, Vec<(String, usize)>) {
    let mut sites = Sites { text: &src.text, function: String::new(), out: Vec::new() };
    let mut allowed = Vec::new();
    for item in &file.items {
        let Item::Fn(d) = item else { continue };
        if d.external.is_some() {
            continue;
        }
        let name = match &d.receiver {
            Some(r) => format!("{}.{}", r.ty.name, d.name.name),
            None => d.name.name.clone(),
        };
        if only.is_some_and(|o| !o.contains(&name)) {
            continue;
        }
        if d.allows.iter().any(|a| a.lint.name == "mutants") {
            allowed.push((name.clone(), d.name.span.start));
        }
        sites.function = name;
        sites.block(&d.body);
    }
    sites.out.sort_by_key(|m| (m.start, m.end));
    (sites.out, allowed)
}

/// A program run for the mutants: its tests, its golden (if it has a main and one), and the
/// steps any run of it may take.
struct Target {
    file: String,
    golden: Option<(Output, Input)>,
    budget: u64,
}

/// What a program's tests and golden say: None if they all pass, else what noticed.
fn judge(project: &Project, target: &Target, program: Program, steps: u64) -> Result<Option<String>, String> {
    let mut ffi = crate::c_functions(&program)?;
    let root = project.root.clone();
    let p = program.clone();
    let ran = anvil_interp::run_on_big_stack(move || {
        crate::testing::run_all(&p, crate::PROPERTY_CASES, 0, Some(steps), None, &mut ffi, &root)
    });
    if let Some(r) = ran.iter().find(|r| r.failure.is_some()) {
        return Ok(Some(format!("test \"{}\" in {}", r.name, target.file)));
    }
    if let Some((want, input)) = &target.golden {
        match golden::interpret(program, Some(steps), &project.root, input) {
            Err(_) => return Ok(Some(format!("the golden of {} (it ran out of steps)", target.file))),
            Ok((got, _)) => {
                if golden::difference(want, &got).is_some() {
                    return Ok(Some(format!("the golden of {}", target.file)));
                }
            }
        }
    }
    Ok(None)
}

fn compile(files: Files, file: &str) -> Result<Program, String> {
    let src = files.load(file).map_err(|r| r.json["error"].as_str().unwrap_or("").to_string())?;
    match anvil_front::compile_with(&src, &files.loader()) {
        (_, Ok(p)) => Ok(p),
        (_, Err(d)) => Err(format!("{} error(s), the first {}: {}", d.len(), d[0].code, d[0].message)),
    }
}

/// The baseline of one program: its tests and golden must pass unchanged, and the steps they
/// take bound what a mutant may take (ten times as many, so a mutant that loops is noticed).
fn baseline(project: &Project, file: &str, program: Program) -> Result<Target, String> {
    let golden = match (program.main, golden::read(project, file)?) {
        (Some(_), Some(want)) => Some((want, golden::read_input(project, file)?)),
        _ => None,
    };
    let mut ffi = crate::c_functions(&program)?;
    let root = project.root.clone();
    let p = program.clone();
    let (ran, steps) = anvil_interp::run_on_big_stack(move || {
        let mut rec = anvil_interp::Recorder::new(&p);
        let ran = crate::testing::run_all(&p, crate::PROPERTY_CASES, 0, None, Some(&mut rec), &mut ffi, &root);
        (ran, rec.steps)
    });
    if let Some(r) = ran.iter().find(|r| r.failure.is_some()) {
        return Err(format!("test \"{}\" in {file} fails before any change; fix it first", r.name));
    }
    let mut total = steps;
    if let Some((want, input)) = &golden {
        let (got, counts) = golden::interpret(program, None, &project.root, input)?;
        if golden::difference(want, &got).is_some() {
            return Err(format!("{file} does not match its golden before any change; fix it first"));
        }
        total += counts.steps;
    }
    Ok(Target { file: file.to_string(), golden, budget: total.saturating_mul(10).saturating_add(10_000) })
}

pub fn mutate(root: &Path, file: &str, only: Option<&str>, max_steps: Option<u64>) -> Report {
    let project = match Project::discover(root) {
        Ok(p) => p,
        Err(e) => return Report::usage(format!("{e}; mutate reruns a project's tests and goldens")),
    };
    let file = file.replace('\\', "/");
    let disk = Files::disk(&project.root);
    let src = match disk.load(&file) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let ast = match anvil_front::parse(&src) {
        Ok(a) => a,
        Err(d) => return crate::diagnostics(std::slice::from_ref(&src), &d),
    };
    let only: Option<Vec<String>> = only.map(|o| o.split(',').map(|s| s.trim().to_string()).collect());
    let (list, allowed) = mutants(&src, &ast, only.as_deref());
    if let Some(names) = &only {
        let known: Vec<String> = mutants(&src, &ast, None).0.into_iter().map(|m| m.function).collect();
        if let Some(missing) = names.iter().find(|n| !known.contains(n)) {
            return Report::usage(format!("{file} has no function `{missing}` with anything to mutate"));
        }
    }
    // The file itself, then every project file whose program includes it.
    let mut targets = Vec::new();
    let mut names = vec![file.clone()];
    for f in &project.files {
        if *f == file {
            continue;
        }
        if let Ok(p) = compile(disk, f) {
            if p.modules.iter().any(|m| *m == file) {
                names.push(f.clone());
            }
        }
    }
    for f in &names {
        let program = match compile(disk, f) {
            Ok(p) => p,
            Err(e) => return Report::usage(format!("{f} does not compile before any change ({e}); fix it first")),
        };
        match baseline(&project, f, program) {
            Ok(t) => targets.push(t),
            Err(e) => return Report::usage(e),
        }
    }
    let mut results = Vec::new();
    let mut text = String::new();
    let (mut killed, mut survived, mut invalid, mut excused) = (0, 0, 0, 0);
    let mut survivors_in: HashMap<&str, usize> = HashMap::new();
    for m in &list {
        let mut changed = String::with_capacity(src.text.len());
        changed.push_str(&src.text[..m.start]);
        changed.push_str(&m.text);
        changed.push_str(&src.text[m.end..]);
        // In canonical layout, so a left-out line is not a layout error.
        if let Ok(t) = anvil_front::fmt::format(&SourceFile::new(file.clone(), changed.clone())) {
            changed = t;
        }
        let open: HashMap<String, String> = [(file.clone(), changed)].into();
        let files = Files { root: &project.root, open: Some(&open) };
        let (l, c) = src.line_col(m.start);
        let mut result = json!({
            "at": format!("{l}:{c}"), "function": m.function, "from": m.from, "to": m.to,
        });
        let mut status = "survived";
        let mut by: Option<String> = None;
        for t in &targets {
            let program = match compile(files, &t.file) {
                Ok(p) => p,
                Err(_) if t.file == file => {
                    status = "invalid";
                    break;
                }
                // It compiles alone but breaks an importer: that importer noticed.
                Err(e) => {
                    status = "killed";
                    by = Some(format!("{} no longer compiles ({e})", t.file));
                    break;
                }
            };
            match judge(&project, t, program, max_steps.unwrap_or(t.budget)) {
                Ok(None) => {}
                Ok(Some(what)) => {
                    status = "killed";
                    by = Some(what);
                    break;
                }
                Err(e) => return Report::error(e),
            }
        }
        if status == "survived" {
            *survivors_in.entry(m.function.as_str()).or_default() += 1;
            if allowed.iter().any(|(f, _)| *f == m.function) {
                status = "allowed";
            }
        }
        match status {
            "killed" => killed += 1,
            "invalid" => invalid += 1,
            "allowed" => excused += 1,
            _ => {
                survived += 1;
                text.push_str(&format!(
                    "{file}:{l}:{c} survived: `{}` to `{}` in `{}`; no test or golden noticed\n",
                    m.from, m.to, m.function
                ));
            }
        }
        result["status"] = json!(status);
        if let Some(b) = by {
            result["by"] = json!(b);
        }
        results.push(result);
    }
    // An allow that excuses nothing is stale, like the lints' (only for functions mutated here).
    let mut stale = Vec::new();
    for (f, offset) in &allowed {
        if !survivors_in.contains_key(f.as_str()) && list.iter().any(|m| m.function == *f) {
            let (l, c) = src.line_col(*offset);
            text.push_str(&format!(
                "{file}:{l}:{c} stale: every mutant of `{f}` is killed, so its `allow mutants` is stale: remove it\n"
            ));
            stale.push(json!({ "function": f, "at": format!("{l}:{c}") }));
        }
    }
    let valid = killed + survived + excused;
    let score = if valid == 0 { 100 } else { killed * 100 / valid };
    text.push_str(&format!(
        "{file}: {} mutant(s): {killed} killed, {survived} survived, {excused} allowed to survive, {invalid} did not \
         compile; {score}% killed (run against {})\n",
        list.len(),
        names.join(", ")
    ));
    let passed = survived == 0 && stale.is_empty();
    let json = json!({
        "ok": passed, "file": file, "mutants": results, "killed": killed, "survived": survived,
        "allowed": excused, "invalid": invalid, "score": score, "against": names, "stale_allows": stale,
    });
    let exit = if passed { EXIT_OK } else { EXIT_FAILED };
    Report { json, stdout: text, stderr: String::new(), exit, failed: !passed }
}
