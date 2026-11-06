//! `anvil mutate FILE`: does anything notice when the code is wrong? Each mutant changes one
//! thing in one function or constant of FILE (an operator, a literal, a call statement), and is run against
//! the tests and goldens of FILE and of every project file that imports it that enter the mutated
//! function; the others go as they did. A mutant nothing notices "survives": a test is missing,
//! or asserts too little. Some survivors change nothing a
//! program can see (`a < b` for `a <= b` in a min); `allow mutants "why"` before the function or
//! one statement says so, and is reported as stale when nothing in it survives.

use crate::golden::{self, Input, Output};
use crate::project::Project;
use crate::{Files, Report, EXIT_FAILED, EXIT_OK};
use anvil_front::ast::{self, BinOp, Else, ExprKind, InterpPart, Item, StmtKind, UnOp};
use anvil_front::ir::Program;
use anvil_front::span::SourceFile;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// One change to the source: replace `start..end` with `text`.
struct Mutant {
    start: usize,
    end: usize,
    text: String,
    /// What was there and what it became, as the report shows them.
    from: String,
    to: String,
    function: String,
    /// Where its function is: from the name to the end of the body. `EVERYWHERE` for a constant,
    /// which any run may read.
    decl: (usize, usize),
}

const EVERYWHERE: (usize, usize) = (0, usize::MAX);

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
    decl: (usize, usize),
    out: Vec<Mutant>,
    /// The statements with an `allow mutants`, as the spans of what they excuse.
    excused: Vec<(usize, usize)>,
}

impl Sites<'_> {
    fn add(&mut self, start: usize, end: usize, text: String, from: &str, to: &str) {
        let function = self.function.clone();
        self.out.push(Mutant { start, end, text, from: from.into(), to: to.into(), function, decl: self.decl });
    }

    fn block(&mut self, b: &ast::Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        if !s.allows.is_empty() {
            self.excused.push((s.span.start, s.span.end));
        }
        match &s.kind {
            StmtKind::Let { init, otherwise, .. } => {
                self.expr(init);
                if let Some(other) = otherwise {
                    self.block(&other.body);
                }
            }
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
            ExprKind::If(c, a, b) => {
                self.expr(c);
                self.expr(a);
                self.expr(b);
            }
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

/// The mutants of a file, and what excuses some of them.
struct Found {
    list: Vec<Mutant>,
    /// The functions that allow mutants to survive, with the offset of their names.
    allowed: Vec<(String, usize)>,
    /// The statements that do.
    excused: Vec<(usize, usize)>,
}

/// The mutants of the file's functions and constants (of those in `only`).
fn mutants(src: &SourceFile, file: &ast::File, only: Option<&[String]>) -> Found {
    let mut sites = Sites { text: &src.text, function: String::new(), decl: (0, 0), out: Vec::new(), excused: Vec::new() };
    let mut allowed = Vec::new();
    for item in &file.items {
        if let Item::Const(c) = item {
            if only.is_none_or(|o| o.contains(&c.name.name)) {
                sites.function = c.name.name.clone();
                sites.decl = EVERYWHERE;
                sites.expr(&c.value);
            }
        }
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
        sites.decl = (d.name.span.start, d.body.span.end);
        sites.block(&d.body);
    }
    sites.out.sort_by_key(|m| (m.start, m.end));
    Found { list: sites.out, allowed, excused: sites.excused }
}

/// One run of a program, as it went unchanged: a test, or its golden (`test` is None).
struct Run {
    test: Option<usize>,
    steps: u64,
    /// The functions it entered, as (file, offset of the name). A run that enters none of the
    /// mutated function goes exactly as it did, so it cannot notice the mutant.
    reached: Vec<(String, usize)>,
}

impl Run {
    fn sees(&self, file: &str, decl: (usize, usize)) -> bool {
        decl == EVERYWHERE || self.reached.iter().any(|(f, at)| f == file && decl.0 <= *at && *at < decl.1)
    }

    /// What a mutant may take here: ten times as many steps, so one that loops is noticed.
    fn budget(&self, max_steps: Option<u64>) -> u64 {
        max_steps.unwrap_or(self.steps.saturating_mul(10).saturating_add(10_000))
    }
}

/// A program run for the mutants: its golden if it has a main and one, and its runs, cheapest
/// first.
struct Target {
    file: String,
    golden: Option<(Output, Input)>,
    runs: Vec<Run>,
}

/// What the runs of a program that can see the mutant say: None if they all pass, else what
/// noticed. Stops at the first that notices.
fn judge(
    project: &Project,
    target: &Target,
    program: Program,
    file: &str,
    decl: (usize, usize),
    max_steps: Option<u64>,
) -> Result<Option<String>, String> {
    let seeing: Vec<&Run> = target.runs.iter().filter(|r| r.sees(file, decl)).collect();
    let tests: Vec<(usize, u64)> =
        seeing.iter().filter_map(|r| r.test.map(|t| (t, r.budget(max_steps)))).collect();
    if !tests.is_empty() {
        let mut ffi = crate::c_functions(&program)?;
        let root = project.root.clone();
        let p = program.clone();
        let noticed = anvil_interp::run_on_big_stack(move || {
            tests.into_iter().find_map(|(t, steps)| {
                let r = crate::testing::run_one(&p, t, crate::PROPERTY_CASES, 0, Some(steps), None, &mut ffi, &root);
                r.failure.map(|_| (r.name, r.stopped))
            })
        });
        if let Some((name, stopped)) = noticed {
            let how = if stopped { " (it ran out of steps)" } else { "" };
            return Ok(Some(format!("test \"{name}\" in {}{how}", target.file)));
        }
    }
    if let (Some((want, input)), Some(run)) = (&target.golden, seeing.iter().find(|r| r.test.is_none())) {
        match golden::interpret(program, Some(run.budget(max_steps)), &project.root, input) {
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

/// The baseline of one program: its tests and golden must pass unchanged, and say what each run
/// takes and reaches.
fn baseline(project: &Project, file: &str, program: Program) -> Result<Target, String> {
    let golden = match (program.main, golden::read(project, file)?) {
        (Some(_), Some(want)) => Some((want, golden::read_input(project, file)?)),
        _ => None,
    };
    let mut ffi = crate::c_functions(&program)?;
    let root = project.root.clone();
    let p = program.clone();
    let ran = anvil_interp::run_on_big_stack(move || {
        let mut out = Vec::new();
        for t in 0..p.tests.len() {
            let mut rec = anvil_interp::Recorder::new(&p);
            let r = crate::testing::run_one(&p, t, crate::PROPERTY_CASES, 0, None, Some(&mut rec), &mut ffi, &root);
            let run = Run { test: Some(t), steps: rec.totals[0], reached: golden::reached(&p, &rec) };
            out.push((r.failure.map(|_| r.name), run));
        }
        out
    });
    let mut runs = Vec::new();
    for (failed, run) in ran {
        if let Some(name) = failed {
            return Err(format!("test \"{name}\" in {file} fails before any change; fix it first"));
        }
        runs.push(run);
    }
    if let Some((want, input)) = &golden {
        let (got, counts, reached) = golden::interpret_reaching(program, None, &project.root, input)?;
        if golden::difference(want, &got).is_some() {
            return Err(format!("{file} does not match its golden before any change; fix it first"));
        }
        runs.push(Run { test: None, steps: counts.steps, reached });
    }
    runs.sort_by_key(|r| r.steps);
    Ok(Target { file: file.to_string(), golden, runs })
}

/// What became of one mutant.
#[derive(Clone)]
struct Verdict {
    status: &'static str,
    by: Option<String>,
}

/// Runs one mutant against the targets that can see it.
fn try_mutant(
    project: &Project,
    src: &SourceFile,
    file: &str,
    m: &Mutant,
    targets: &[Target],
    max_steps: Option<u64>,
) -> Result<Verdict, String> {
    let mut changed = String::with_capacity(src.text.len());
    changed.push_str(&src.text[..m.start]);
    changed.push_str(&m.text);
    changed.push_str(&src.text[m.end..]);
    // In canonical layout, so a left-out line is not a layout error.
    if let Ok(t) = anvil_front::fmt::format(&SourceFile::new(file.to_string(), changed.clone())) {
        changed = t;
    }
    let open: HashMap<String, String> = [(file.to_string(), changed)].into();
    let files = Files { root: &project.root, open: Some(&open) };
    for t in targets {
        // Whether the file still compiles is always asked; another program only if it can tell.
        if t.file != file && !t.runs.iter().any(|r| r.sees(file, m.decl)) {
            continue;
        }
        let program = match compile(files, &t.file) {
            Ok(p) => p,
            Err(_) if t.file == file => return Ok(Verdict { status: "invalid", by: None }),
            // It compiles alone but breaks an importer: that importer noticed.
            Err(e) => {
                return Ok(Verdict { status: "killed", by: Some(format!("{} no longer compiles ({e})", t.file)) })
            }
        };
        if let Some(what) = judge(project, t, program, file, m.decl, max_steps)? {
            return Ok(Verdict { status: "killed", by: Some(what) });
        }
    }
    Ok(Verdict { status: "survived", by: None })
}

/// Workers for the mutants: `ANVIL_THREADS` or the machine's cores, at most 8 (each may hold what
/// its runs allocate), and one when C functions are involved, which may not expect threads.
fn workers(programs: &[Program], mutants: usize) -> usize {
    if programs.iter().any(|p| p.funcs.iter().any(|f| f.external.is_some())) {
        return 1;
    }
    let wanted = std::env::var("ANVIL_THREADS").ok().and_then(|v| v.parse::<usize>().ok());
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    wanted.unwrap_or(cores).clamp(1, 8).min(mutants.max(1))
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
    let Found { list, allowed, excused } = mutants(&src, &ast, only.as_deref());
    if let Some(names) = &only {
        let known: Vec<String> = mutants(&src, &ast, None).list.into_iter().map(|m| m.function).collect();
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
    let mut programs = Vec::new();
    for f in &names {
        let program = match compile(disk, f) {
            Ok(p) => p,
            Err(e) => return Report::usage(format!("{f} does not compile before any change ({e}); fix it first")),
        };
        programs.push(program.clone());
        match baseline(&project, f, program) {
            Ok(t) => targets.push(t),
            Err(e) => return Report::usage(e),
        }
    }
    // Mutants run side by side and report as they end; their results keep source order.
    let threads = workers(&programs, list.len());
    drop(programs);
    let total = list.len();
    let show = crate::PROGRESS.load(Ordering::Relaxed);
    if show {
        eprintln!("mutate: {total} mutant(s) of {file}, against {}, on {threads} thread(s)", names.join(", "));
    }
    let next = AtomicUsize::new(0);
    let finished = Mutex::new((vec![None; total], 0usize, 0usize, Instant::now()));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(m) = list.get(k) else { break };
                let v = try_mutant(&project, &src, &file, m, &targets, max_steps);
                let mut f = finished.lock().expect("a mutant's worker crashed");
                f.1 += 1;
                if show && matches!(&v, Ok(r) if r.status == "survived") {
                    f.2 += 1;
                    let (l, c) = src.line_col(m.start);
                    eprintln!("mutate: {file}:{l}:{c} survived: `{}` to `{}` in `{}`", m.from, m.to, m.function);
                }
                if show && f.3.elapsed().as_secs() >= 15 {
                    f.3 = Instant::now();
                    eprintln!("mutate: {} of {total} done, {} survived", f.1, f.2);
                }
                f.0[k] = Some(v);
            });
        }
    });
    let verdicts = finished.into_inner().expect("a mutant's worker crashed").0;
    let mut results = Vec::new();
    let mut text = String::new();
    let (mut killed, mut survived, mut invalid, mut allowed_to_survive) = (0, 0, 0, 0);
    let mut survivors_in: HashMap<&str, usize> = HashMap::new();
    let mut useful = vec![false; excused.len()];
    for (m, v) in list.iter().zip(verdicts) {
        let v = match v.expect("every mutant ran") {
            Ok(v) => v,
            Err(e) => return Report::error(e),
        };
        let (l, c) = src.line_col(m.start);
        let mut result = json!({
            "at": format!("{l}:{c}"), "function": m.function, "from": m.from, "to": m.to,
        });
        let mut status = v.status;
        if status == "survived" {
            let on_it: Vec<usize> =
                (0..excused.len()).filter(|k| excused[*k].0 <= m.start && m.end <= excused[*k].1).collect();
            on_it.iter().for_each(|k| useful[*k] = true);
            if !on_it.is_empty() {
                status = "allowed";
            } else {
                *survivors_in.entry(m.function.as_str()).or_default() += 1;
                if allowed.iter().any(|(f, _)| *f == m.function) {
                    status = "allowed";
                }
            }
        }
        match status {
            "killed" => killed += 1,
            "invalid" => invalid += 1,
            "allowed" => allowed_to_survive += 1,
            _ => {
                survived += 1;
                text.push_str(&format!(
                    "{file}:{l}:{c} survived: `{}` to `{}` in `{}`; no test or golden noticed\n",
                    m.from, m.to, m.function
                ));
            }
        }
        result["status"] = json!(status);
        if let Some(b) = v.by {
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
    for (span, _) in excused.iter().zip(&useful).filter(|(_, u)| !**u) {
        let (l, c) = src.line_col(span.0);
        text.push_str(&format!("{file}:{l}:{c} stale: every mutant of this statement is killed, so its `allow mutants` is stale: remove it\n"));
        stale.push(json!({ "function": "", "at": format!("{l}:{c}") }));
    }
    let valid = killed + survived + allowed_to_survive;
    let score = if valid == 0 { 100 } else { killed * 100 / valid };
    text.push_str(&format!(
        "{file}: {} mutant(s): {killed} killed, {survived} survived, {allowed_to_survive} allowed to survive, {invalid} did not \
         compile; {score}% killed (run against {})\n",
        list.len(),
        names.join(", ")
    ));
    let passed = survived == 0 && stale.is_empty();
    let json = json!({
        "ok": passed, "file": file, "mutants": results, "killed": killed, "survived": survived,
        "allowed": allowed_to_survive, "invalid": invalid, "score": score, "against": names, "stale_allows": stale,
    });
    let exit = if passed { EXIT_OK } else { EXIT_FAILED };
    Report { json, stdout: text, stderr: String::new(), exit, failed: !passed }
}
