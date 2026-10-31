//! Every operation of the `anvil` command, shared by the CLI and the MCP server. Each returns a
//! `Report` whose JSON is exactly what both faces send, so they cannot drift apart. `COMMANDS`
//! is the one list of commands and their arguments: the CLI parses with it and a test checks
//! that the MCP server has a tool for each, with the same parameters.

mod fuzz;
pub mod golden;
mod inspect;
mod proj;
pub mod project;
mod testing;

pub use fuzz::{fuzz, program_for};
pub use inspect::{profile, trace, TRACE_LIMIT};
pub use proj::{
    fmt, golden_approve, golden_check, list, record_decide, record_export, record_import_roadmap, record_query,
    record_status, review, verify, watch,
};
pub use testing::{test, PROPERTY_CASES};

use anvil_front::check::{Analysis, Def};
use anvil_front::diag::{self, Diagnostic, FixKind};
use anvil_front::ir::{Program, PANIC_EXIT_CODE};
use anvil_front::span::{SourceFile, Span};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One command: `anvil NAME positionals... [--options]`; over MCP the tool NAME with `-` as `_`.
pub struct Command {
    pub name: &'static str,
    /// Positional arguments in order; a trailing `?` marks an optional one.
    pub positionals: &'static [&'static str],
    /// Options: (name, value placeholder or "" for a flag, help).
    pub options: &'static [(&'static str, &'static str, &'static str)],
    pub summary: &'static str,
}

/// `--json` exists only in the CLI: MCP tools always answer in JSON.
pub const CLI_ONLY_OPTIONS: &[&str] = &["json"];

/// Commands with no MCP tool, and why.
pub const CLI_ONLY_COMMANDS: &[(&str, &str)] =
    &[("watch", "it runs until stopped; an MCP client calls verify after each change instead")];

pub const COMMANDS: &[Command] = &[
    Command { name: "check", positionals: &["file"], options: &[], summary: "type-check a file" },
    Command {
        name: "run",
        positionals: &["file"],
        options: &[("max-steps", "N", "stop after N statements and loop iterations")],
        summary: "interpret main (the reference semantics)",
    },
    Command {
        name: "build",
        positionals: &["file"],
        options: &[
            ("out", "PATH", "executable to write (default: the file with .exe)"),
            ("asm", "", "include each function's machine code"),
        ],
        summary: "compile main to a native executable",
    },
    Command {
        name: "test",
        positionals: &["file"],
        options: &[
            ("max-steps", "N", "stop each test (or property case) after N steps"),
            ("native", "", "also run the plain tests as a native executable; it must agree"),
            ("cases", "N", "cases per property test (default 100)"),
            ("seed", "N", "seed for property tests' inputs (default 0)"),
        ],
        summary: "run every test block; `for all` tests on generated inputs",
    },
    Command {
        name: "fmt",
        positionals: &["file?"],
        options: &[("check", "", "change nothing; fail if a file is not in canonical layout")],
        summary: "put the file (or every project file) in canonical layout",
    },
    Command {
        name: "fix",
        positionals: &["file"],
        options: &[],
        summary: "apply every mechanical fix to the file, repeating until none is left",
    },
    Command {
        name: "outline",
        positionals: &["file"],
        options: &[],
        summary: "list the file's functions, types, units and tests with their signatures",
    },
    Command {
        name: "type-at",
        positionals: &["file", "line", "col"],
        options: &[],
        summary: "the type of the expression and the declaration of the name at a position",
    },
    Command {
        name: "refs",
        positionals: &["file", "name"],
        options: &[],
        summary: "where a function, type, field (Type.field), variant (Enum.Variant) or unit is declared and used",
    },
    Command {
        name: "explain",
        positionals: &["code?"],
        options: &[],
        summary: "explain an error code (no code: list all)",
    },
    Command { name: "spec", positionals: &[], options: &[], summary: "the language specification" },
    Command {
        name: "trace",
        positionals: &["file"],
        options: &[
            ("test", "NAME", "trace this test instead of main"),
            ("only", "NAMES", "keep the calls of these functions only (comma-separated); prints are always kept"),
            ("limit", "N", "keep at most N events (default 1000)"),
            STEPS,
        ],
        summary: "a timeline of calls, arguments, returns and prints",
    },
    Command {
        name: "fuzz",
        positionals: &[],
        options: &[
            ("seed", "N", "seed of the first program (default 1); program N is always the same"),
            ("count", "N", "programs to try (default 10)"),
        ],
        summary: "random well-typed programs on both backends; any difference is a backend bug",
    },
    Command {
        name: "profile",
        positionals: &["file"],
        options: &[("test", "NAME", "profile this test instead of main"), STEPS],
        summary: "calls and steps per function",
    },
    // Project commands: they find anvil.toml in the current directory or above.
    Command {
        name: "list",
        positionals: &[],
        options: &[],
        summary: "the project's files: functions, tests, main, golden",
    },
    Command {
        name: "verify",
        positionals: &[],
        options: &[STEPS, NATIVE],
        summary: "check, test and golden-check every project file; store the findings in the record",
    },
    Command {
        name: "review",
        positionals: &[],
        options: &[STEPS, NATIVE],
        summary: "verify, then write out/review.md, the sheet a person signs off",
    },
    Command {
        name: "golden check",
        positionals: &["file?"],
        options: &[STEPS, NATIVE],
        summary: "compare programs' output, exit code and panic with their approved goldens",
    },
    Command {
        name: "golden approve",
        positionals: &["file"],
        options: &[STEPS],
        summary: "store a program's current output as its golden (look at it first)",
    },
    Command {
        name: "record query",
        positionals: &["kind?"],
        options: &[("open", "", "only open findings")],
        summary: "the project record: milestones, decisions, findings or runs (none: a summary)",
    },
    Command {
        name: "record decide",
        positionals: &["title", "body?"],
        options: &[],
        summary: "record a decision, dated today",
    },
    Command {
        name: "record status",
        positionals: &["milestone", "status"],
        options: &[],
        summary: "set a milestone's status: open, active, done or dropped",
    },
    Command {
        name: "record import-roadmap",
        positionals: &["file?"],
        options: &[],
        summary: "load milestones from a roadmap (default docs/ROADMAP.md)",
    },
    Command { name: "record export", positionals: &[], options: &[], summary: "the whole record as Markdown" },
    Command {
        name: "watch",
        positionals: &[],
        options: &[NATIVE, STEPS, ("runs", "N", "stop after N verify passes")],
        summary: "verify, and again after every change (CLI only)",
    },
];

const STEPS: (&str, &str, &str) = ("max-steps", "N", "stop each run after N statements and loop iterations");
const NATIVE: (&str, &str, &str) = ("native", "", "also build and run each program natively");

pub const SPEC: &str = include_str!("../../../docs/SPEC.md");

/// CLI exit codes: 0 success, 1 errors or failures, 2 usage errors. `run` passes through the
/// program's own exit code (101 on panic).
pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;

/// What an operation produced.
pub struct Report {
    /// What `--json` prints and what the MCP tool returns.
    pub json: Value,
    /// Human-readable output for the CLI without `--json`.
    pub stdout: String,
    pub stderr: String,
    /// The CLI's exit code.
    pub exit: u8,
    /// Whether this counts as a failure: a non-zero exit in the CLI, a tool error over MCP.
    pub failed: bool,
}

impl Report {
    fn ok(json: Value, stdout: String) -> Report {
        Report { json, stdout, stderr: String::new(), exit: EXIT_OK, failed: false }
    }
    fn failed(json: Value, stderr: String, exit: u8) -> Report {
        Report { json, stdout: String::new(), stderr, exit, failed: true }
    }
    pub fn usage(message: impl Into<String>) -> Report {
        let message = message.into();
        Report::failed(json!({ "ok": false, "error": message }), format!("error: {message}\n"), EXIT_USAGE)
    }
}

/// A source file named as the user gave it (diagnostics cite this name), read relative to `root`.
pub fn load(root: &Path, file: &str) -> Result<SourceFile, Report> {
    let path = root.join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(SourceFile::new(file.replace('\\', "/"), text)),
        Err(e) => Err(Report::usage(format!("cannot read {file}: {e}"))),
    }
}

fn pos(src: &SourceFile, offset: usize) -> Value {
    let (line, col) = src.line_col(offset);
    json!({ "line": line, "col": col })
}

/// Reads a program's modules from under `root`, where every file name is relative to it.
pub(crate) fn loader(root: &Path) -> impl Fn(&str) -> Result<String, String> + '_ {
    move |name: &str| std::fs::read_to_string(root.join(name)).map_err(|e| e.to_string())
}

/// Compile errors as a failed report; `sources` are the program's files, which each
/// diagnostic's `file` indexes.
pub(crate) fn diagnostics(sources: &[SourceFile], diags: &[Diagnostic]) -> Report {
    let ds: Vec<_> = diags.iter().map(|d| diag::to_json(&sources[d.file], d)).collect();
    let mut text = String::new();
    for d in diags {
        text.push_str(&diag::render(&sources[d.file], d));
        text.push('\n');
    }
    text.push_str(&format!("{} error(s)\n", diags.len()));
    Report::failed(json!({ "ok": false, "diagnostics": ds }), text, EXIT_FAILED)
}

/// The program rooted at `file`, with the modules it imports read from under `root`.
fn compile(root: &Path, file: &str) -> Result<(SourceFile, Program), Report> {
    let src = load(root, file)?;
    match anvil_front::compile_with(&src, &loader(root)) {
        (_, Ok(p)) => Ok((src, p)),
        (sources, Err(d)) => Err(diagnostics(&sources, &d)),
    }
}

fn with_main(root: &Path, file: &str) -> Result<(SourceFile, Program), Report> {
    let (src, program) = compile(root, file)?;
    if program.main.is_none() {
        let d = Diagnostic::new("E0350", Span::new(0, 0), "this file has no main function");
        return Err(diagnostics(std::slice::from_ref(&src), &[d]));
    }
    Ok((src, program))
}

macro_rules! try_report {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

pub fn check(root: &Path, file: &str) -> Report {
    try_report!(compile(root, file));
    Report::ok(json!({ "ok": true, "diagnostics": [] }), String::new())
}

/// Interprets main, writing its output to `out` as it runs. The report's JSON holds the exit
/// code and, if it stopped early, the panic or the step limit.
pub fn run(root: &Path, file: &str, max_steps: Option<u64>, out: &mut (dyn Write + Send)) -> Report {
    let (_, program) = try_report!(with_main(root, file));
    // A thread with a stack big enough for MAX_CALL_DEPTH interpreted calls; output streams.
    let outcome = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(s, || anvil_interp::run_main(&program, out, max_steps))
            .expect("cannot spawn interpreter thread")
            .join()
            .expect("interpreter thread crashed")
    });
    let _ = out.flush();
    match outcome {
        anvil_interp::Outcome::Exit(code) => Report {
            json: json!({ "ok": true, "exit": code }),
            stdout: String::new(),
            stderr: String::new(),
            exit: code as u8,
            failed: false,
        },
        anvil_interp::Outcome::Panicked(m) => Report::failed(
            json!({ "ok": false, "exit": PANIC_EXIT_CODE, "panic": m }),
            format!("panic: {m}\n"),
            PANIC_EXIT_CODE as u8,
        ),
        anvil_interp::Outcome::OutOfSteps(n) => {
            let m = anvil_interp::step_limit_message(n);
            Report::failed(json!({ "ok": false, "stopped": m }), format!("error: {m}\n"), EXIT_FAILED)
        }
    }
}

pub fn build(root: &Path, file: &str, out: Option<&str>, asm: bool) -> Report {
    let (_, program) = try_report!(with_main(root, file));
    let out: PathBuf = match out {
        Some(o) => root.join(o),
        None => root.join(file).with_extension("exe"),
    };
    let work = std::env::temp_dir().join(format!("anvil_build_{}_{}", std::process::id(), unique()));
    let result = anvil_codegen::build_exe(&program, &out, &work, asm);
    let _ = std::fs::remove_dir_all(&work);
    match result {
        Ok(listing) => {
            let mut v = json!({ "ok": true, "output": out.display().to_string() });
            if asm {
                v["asm"] = json!(listing);
            }
            Report::ok(v, if asm { listing } else { String::new() })
        }
        Err(e) => Report::failed(json!({ "ok": false, "error": e }), format!("error: {e}\n"), EXIT_FAILED),
    }
}

/// Distinct per call within this process, for scratch directories (the MCP server builds many times).
pub(crate) fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Applies every mechanical fix in `file`, rechecks, and repeats until none is left (at most 20
/// rounds). Fixes that overlap one already applied in the same round wait for the next round.
/// Errors in imported modules are reported, never fixed from here.
pub fn fix(root: &Path, file: &str) -> Report {
    const MAX_ROUNDS: usize = 20;
    let path = root.join(file);
    let mut applied = Vec::new();
    let mut rounds = 0;
    loop {
        let src = try_report!(load(root, file));
        let (sources, a) = anvil_front::analyze_with(&src, &loader(root));
        let mut edits: Vec<(Span, String, String, &'static str)> = Vec::new();
        for d in a.diags.iter().filter(|d| d.file == 0) {
            for f in d.fixes.iter().filter(|f| f.kind == FixKind::Mechanical) {
                for e in &f.edits {
                    edits.push((e.span, e.text.clone(), f.description.clone(), d.code));
                }
            }
        }
        if edits.is_empty() || rounds == MAX_ROUNDS {
            let remaining: Vec<_> = a.diags.iter().map(|d| diag::to_json(&sources[d.file], d)).collect();
            let ok = a.diags.is_empty();
            let json = json!({ "ok": ok, "applied": applied, "rounds": rounds, "diagnostics": remaining });
            let mut text = format!("applied {} fix(es) in {rounds} round(s)\n", applied.len());
            if ok {
                return Report::ok(json, text);
            }
            for d in &a.diags {
                text.push_str(&diag::render(&sources[d.file], d));
                text.push('\n');
            }
            text.push_str(&format!("{} error(s) left; they need a decision, not a mechanical fix\n", a.diags.len()));
            return Report::failed(json, text, EXIT_FAILED);
        }
        // Last edit first, so earlier offsets stay valid; skip any that overlap one already taken.
        edits.sort_by(|x, y| y.0.start.cmp(&x.0.start).then(y.0.end.cmp(&x.0.end)));
        let mut text = src.text.clone();
        let mut floor = usize::MAX;
        for (span, new, description, code) in edits {
            if span.end > floor || (span.end == floor && span.start < span.end) {
                continue;
            }
            applied.push(json!({ "code": code, "fix": description, "at": pos(&src, span.start) }));
            text.replace_range(span.start..span.end, &new);
            floor = span.start;
        }
        if let Err(e) = std::fs::write(&path, text) {
            return Report::usage(format!("cannot write {file}: {e}"));
        }
        rounds += 1;
    }
}

/// Collapses runs of whitespace, so a multi-line signature reads as one line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn outline(root: &Path, file: &str) -> Report {
    use anvil_front::ast::Item;
    let src = try_report!(load(root, file));
    let ast = match anvil_front::parse(&src) {
        Ok(f) => f,
        Err(d) => return diagnostics(std::slice::from_ref(&src), &d),
    };
    let text = |s: Span| src.slice(s).to_string();
    let line = |s: Span| src.line_col(s.start).0;
    let mut items = Vec::new();
    let mut human = String::new();
    for item in &ast.items {
        let v = match item {
            Item::Import(i) => {
                human.push_str(&format!("{:>4}  {}\n", line(i.span), one_line(&text(i.span))));
                let names: Vec<&str> = i.names.iter().map(|n| n.name.as_str()).collect();
                let file = anvil_front::module_file(&src.name, &i.module());
                json!({ "kind": "import", "module": i.module(), "file": file, "names": names, "line": line(i.span) })
            }
            Item::Fn(f) => {
                let sig = one_line(&text(Span::new(f.span.start, f.body.span.start)));
                for a in &f.allows {
                    human.push_str(&format!("{:>4}  {}\n", line(a.span), text(a.span)));
                }
                human.push_str(&format!("{:>4}  {sig}\n", line(f.span)));
                let allows: Vec<_> =
                    f.allows.iter().map(|a| json!({ "lint": a.lint.name, "reason": a.reason })).collect();
                json!({
                    "kind": "fn", "name": f.name.name, "pub": f.public, "line": line(f.span), "signature": sig,
                    "uses_io": f.uses.iter().any(|u| u.name == "io"),
                    "requires": f.requires.iter().map(|e| text(e.span)).collect::<Vec<_>>(),
                    "ensures": f.ensures.iter().map(|e| text(e.span)).collect::<Vec<_>>(),
                    "allows": allows,
                })
            }
            Item::Struct(s) => {
                let fields: Vec<_> =
                    s.fields.iter().map(|(f, t)| json!({ "name": f.name, "type": text(t.span) })).collect();
                let list: Vec<String> = s.fields.iter().map(|(f, t)| format!("{}: {}", f.name, text(t.span))).collect();
                human.push_str(&format!("{:>4}  struct {} {{ {} }}\n", line(s.span), s.name.name, list.join(", ")));
                json!({ "kind": "struct", "name": s.name.name, "line": line(s.span), "fields": fields })
            }
            Item::Enum(e) => {
                let variants: Vec<String> = e.variants.iter().map(|v| one_line(&text(v.span))).collect();
                human.push_str(&format!("{:>4}  enum {} {{ {} }}\n", line(e.span), e.name.name, variants.join(", ")));
                json!({ "kind": "enum", "name": e.name.name, "line": line(e.span), "variants": variants })
            }
            Item::Unit(u) => {
                human.push_str(&format!("{:>4}  {}\n", line(u.span), text(u.span)));
                json!({ "kind": "unit", "name": u.name.name, "line": line(u.span), "text": text(u.span) })
            }
            Item::Test(t) => {
                human.push_str(&format!("{:>4}  test \"{}\"\n", line(t.span), t.name));
                json!({ "kind": "test", "name": t.name, "line": line(t.span) })
            }
        };
        items.push(v);
    }
    Report::ok(json!({ "ok": true, "file": src.name, "items": items }), human)
}

/// The innermost entry whose span contains `offset` (ends inclusive, so the end of a name counts).
fn innermost<T>(entries: impl Iterator<Item = (Span, T)>, offset: usize) -> Option<(Span, T)> {
    entries.filter(|(s, _)| s.start <= offset && offset <= s.end).min_by_key(|(s, _)| s.end - s.start)
}

fn def_json(src: &SourceFile, d: &Def) -> Value {
    json!({ "name": d.name, "kind": d.kind, "detail": d.detail, "defined_at": pos(src, d.span.start) })
}

/// Checker facts, or the syntax error that prevented them.
fn analyze(root: &Path, file: &str) -> Result<(SourceFile, Analysis), Report> {
    let src = load(root, file)?;
    let (sources, a) = anvil_front::analyze_with(&src, &loader(root));
    if a.facts.defs.is_empty() && !a.diags.is_empty() {
        // A syntax error stops before checking, so there is nothing to answer from.
        return Err(diagnostics(&sources, &a.diags));
    }
    Ok((src, a))
}

pub fn type_at(root: &Path, file: &str, line: usize, col: usize) -> Report {
    let (src, a) = try_report!(analyze(root, file));
    let Some(offset) = src.offset_of(line, col) else {
        return Report::usage(format!("{file} has no line {line}, column {col}"));
    };
    let f = &a.facts;
    let expr = innermost(f.exprs.iter().map(|(s, t)| (*s, t.clone())), offset);
    // A name at the position: a declaration itself, or a use pointing at one.
    let named = innermost(f.defs.iter().map(|d| (d.span, d.span)), offset)
        .or_else(|| innermost(f.uses.iter().map(|(u, d)| (*u, *d)), offset));
    let def = named.and_then(|(_, def_span)| f.defs.iter().find(|d| d.span == def_span));
    if expr.is_none() && def.is_none() {
        let m = format!("no expression or name at {line}:{col}");
        return Report::failed(json!({ "ok": false, "error": m }), format!("error: {m}\n"), EXIT_FAILED);
    }
    let mut v = json!({ "ok": true, "line": line, "col": col, "errors_in_file": a.diags.len() });
    let mut human = String::new();
    if let Some((span, ty)) = &expr {
        v["text"] = json!(src.slice(*span));
        v["type"] = json!(ty);
        human.push_str(&format!("{}: {ty}\n", src.slice(*span)));
    }
    if let Some(d) = def {
        v["declaration"] = def_json(&src, d);
        let (l, c) = src.line_col(d.span.start);
        human.push_str(&format!("{} {} declared at {}:{l}:{c}: {}\n", d.kind, d.name, src.name, d.detail));
    }
    if !a.diags.is_empty() {
        human.push_str(&format!("(the file has {} error(s); answers may be incomplete)\n", a.diags.len()));
    }
    Report::ok(v, human)
}

pub fn refs(root: &Path, file: &str, name: &str) -> Report {
    let (src, a) = try_report!(analyze(root, file));
    let f = &a.facts;
    let defs: Vec<&Def> = f.defs.iter().filter(|d| d.name == name && !matches!(d.kind, "param" | "local")).collect();
    if defs.is_empty() {
        let mut names: Vec<&str> =
            f.defs.iter().filter(|d| !matches!(d.kind, "param" | "local")).map(|d| d.name.as_str()).collect();
        names.sort();
        names.dedup();
        let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let suggestion = anvil_front::check::closest(name, owned.iter());
        let mut m = format!("nothing called `{name}` is declared in {file}");
        if let Some(s) = &suggestion {
            m.push_str(&format!("; did you mean `{s}`?"));
        }
        let json = json!({ "ok": false, "error": m, "suggestion": suggestion, "declared": names });
        return Report::failed(json, format!("error: {m}\ndeclared: {}\n", names.join(", ")), EXIT_FAILED);
    }
    let mut out = Vec::new();
    let mut human = String::new();
    for d in defs {
        let mut uses: Vec<Span> = f.uses.iter().filter(|(_, def)| *def == d.span).map(|(u, _)| *u).collect();
        uses.sort_by_key(|s| s.start);
        uses.dedup();
        let (l, c) = src.line_col(d.span.start);
        human.push_str(&format!("{} {} declared at {}:{l}:{c}: {}\n", d.kind, d.name, src.name, d.detail));
        let list: Vec<_> = uses
            .iter()
            .map(|u| {
                let (ul, uc) = src.line_col(u.start);
                human.push_str(&format!("  {}:{ul}:{uc}  {}\n", src.name, src.line_text(ul).trim()));
                json!({ "line": ul, "col": uc, "source": src.line_text(ul).trim() })
            })
            .collect();
        human.push_str(&format!("  {} use(s)\n", list.len()));
        let mut v = def_json(&src, d);
        v["uses"] = json!(list);
        out.push(v);
    }
    Report::ok(json!({ "ok": true, "declarations": out, "errors_in_file": a.diags.len() }), human)
}

/// One error code or lint, or every code followed by every lint.
pub fn explain(code: Option<&str>) -> Report {
    match code {
        Some(code) => match diag::explain(code).or_else(|| diag::lint(code)) {
            Some(text) => Report::ok(json!({ "code": code, "explanation": text }), format!("{code}: {text}\n")),
            None => Report::usage(format!("unknown error code or lint `{code}`")),
        },
        None => {
            let entries: Vec<(&str, &str)> = diag::ALL_CODES
                .iter()
                .map(|c| (*c, diag::explain(c).unwrap()))
                .chain(diag::LINTS.iter().copied())
                .collect();
            let all: Vec<_> = entries.iter().map(|(c, t)| json!({ "code": c, "explanation": t })).collect();
            let text: String = entries.iter().map(|(c, t)| format!("{c}: {t}\n")).collect();
            Report::ok(Value::Array(all), text)
        }
    }
}

pub fn spec() -> Report {
    Report::ok(json!({ "spec": SPEC }), SPEC.to_string())
}
