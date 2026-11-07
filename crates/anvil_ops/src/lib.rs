//! Every operation of the `anvil` command, shared by the CLI and the MCP server. Each returns a
//! `Report` whose JSON is exactly what both faces send, so they cannot drift apart. `COMMANDS`
//! is the one list of commands and their arguments: the CLI parses with it and a test checks
//! that the MCP server has a tool for each, with the same parameters.

mod api;
mod cost;
mod docs;
mod fuzz;
pub mod golden;
mod inspect;
mod lsp;
mod compare;
mod hot;
mod measure;
mod mutate;
pub(crate) mod proj;
pub mod project;
mod query;
mod testing;
mod usage;

pub use api::{api, api_approve};
pub use cost::cost;
pub use docs::docs;
pub use fuzz::{fuzz, program_for};
pub use inspect::{profile, trace, TRACE_LIMIT};
pub use lsp::lsp;
pub use compare::compare;
pub use hot::hot;
pub use measure::measure;
pub use mutate::mutate;
pub use proj::{
    fmt, golden_approve, golden_check, list, record_decide, record_export, record_import_roadmap, record_query,
    record_status, review, verify, watch,
};
pub use query::{complete_in, refs_in, type_at_in};
pub use testing::{test, PROPERTY_CASES};
pub use usage::{usage, usage_approve};

/// Whether long commands say how far they are, on stderr (the CLI does; the MCP server's stdio is
/// its protocol's).
pub static PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

use anvil_front::diag::{self, Diagnostic, FixKind};
use anvil_front::ir::{Program, PANIC_EXIT_CODE};
use anvil_front::span::{SourceFile, Span};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// One command: `anvil NAME positionals... [--options]`; over MCP the tool NAME with `-` as `_`.
pub struct Command {
    pub name: &'static str,
    /// Positional arguments in order; a trailing `?` marks an optional one, and a trailing `*`
    /// the program's own arguments, any number of them after `--`.
    pub positionals: &'static [&'static str],
    /// Options: (name, value placeholder or "" for a flag, help).
    pub options: &'static [(&'static str, &'static str, &'static str)],
    pub summary: &'static str,
}

/// `--json` exists only in the CLI: MCP tools always answer in JSON.
pub const CLI_ONLY_OPTIONS: &[&str] = &["json"];

/// Commands with no MCP tool, and why.
pub const CLI_ONLY_COMMANDS: &[(&str, &str)] = &[
    ("watch", "it runs until stopped; an MCP client calls verify after each change instead"),
    ("lsp", "it is a server for editors; an MCP client has the same answers as tools"),
];

pub const COMMANDS: &[Command] = &[
    Command { name: "check", positionals: &["file"], options: &[], summary: "type-check a file" },
    Command {
        name: "run",
        positionals: &["file", "args*"],
        options: &[
            ("max-steps", "N", "stop after N statements and loop iterations"),
            ("input", "FILE", "the program's stdin (default: this command's own; empty over MCP)"),
        ],
        summary: "interpret main (the reference semantics); `-- ARGS` are the program's",
    },
    Command {
        name: "build",
        positionals: &["file"],
        options: &[
            ("out", "PATH", "executable to write (default: the file without .anv; .exe on Windows)"),
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
        summary: "where a function, type, field (Type.field), variant (Enum.Variant) or unit is declared and used; \
                  LINE:COL instead of a name for whatever is declared or used there, locals too",
    },
    Command {
        name: "complete",
        positionals: &["file", "line", "col"],
        options: &[],
        summary: "what can be written at a position: fields and methods after `.`, else every name in scope",
    },
    Command {
        name: "explain",
        positionals: &["code?"],
        options: &[],
        summary: "explain an error code (no code: list all)",
    },
    Command { name: "spec", positionals: &[], options: &[], summary: "the language specification" },
    Command {
        name: "std",
        positionals: &["module?"],
        options: &[],
        summary: "the standard library: its modules and their pub items, or one module's source",
    },
    Command {
        name: "docs",
        positionals: &[],
        options: &[("out", "DIR", "where to write the pages (default out/docs)")],
        summary: "a static site: the specification, every error code and lint, the built-ins",
    },
    Command {
        name: "trace",
        positionals: &["file", "args*"],
        options: &[
            ("test", "NAME", "trace this test instead of main"),
            ("only", "NAMES", "keep the calls of these functions only (comma-separated); prints are always kept"),
            ("limit", "N", "keep at most N events (default 1000)"),
            STEPS,
            INPUT,
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
        name: "cost",
        positionals: &["file"],
        options: &[("only", "NAMES", "measure these functions only (comma-separated)")],
        summary: "how each pure function's steps grow with its arguments, and the steps bound that fits",
    },
    Command {
        name: "profile",
        positionals: &["file", "args*"],
        options: &[("test", "NAME", "profile this test instead of main"), STEPS, INPUT],
        summary: "calls, steps and bytes allocated per function, and where values were copied and why",
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
        name: "mutate",
        positionals: &["file"],
        options: &[
            ("only", "NAMES", "mutate these functions only (comma-separated)"),
            ("max-steps", "N", "stop each run of a mutant after N steps (default: ten times the unchanged run)"),
        ],
        summary: "change one operator, literal or call at a time; report each change no test or golden notices",
    },
    Command {
        name: "api",
        positionals: &["file?"],
        options: &[],
        summary: "a file's public API (or every project file's) against the approved one; any difference fails",
    },
    Command {
        name: "api approve",
        positionals: &["file"],
        options: &[],
        summary: "store a file's current public API as approved (look at the changes first)",
    },
    Command {
        name: "usage",
        positionals: &["file?"],
        options: &[STEPS],
        summary: "the steps, bytes and copied bytes of each run (main, each test) against the approved ones; any growth fails",
    },
    Command {
        name: "usage approve",
        positionals: &["file?"],
        options: &[STEPS],
        summary: "store a file's usage (every project file's that changed) as approved (look at the changes first)",
    },
    Command {
        name: "measure",
        positionals: &["file?", "args*"],
        options: &[STEPS, INPUT, ("native-only", "", "do not run the interpreter too: no steps, nothing compared")],
        summary: "run programs natively: heap bytes asked for, the most held at once, objects and time, beside the interpreter's counts",
    },
    Command {
        name: "hot",
        positionals: &["file", "args*"],
        options: &[INPUT],
        summary: "run a program natively and sample where its time goes: its functions and the runtime's work, and which arrays a loop keeps checking",
    },
    Command {
        name: "compare",
        positionals: &["a", "b", "args*"],
        options: &[
            ("rounds", "N", "how many times to time each, at least 3 (default 7)"),
            ("limit", "SECONDS", "stop a run that takes longer than this, as an error (default 600)"),
            INPUT,
        ],
        summary: "which of two programs runs faster: in turn, on one fast core, and only called a difference when it is more than the runs move on their own; output must match",
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
    Command {
        name: "lsp",
        positionals: &[],
        options: &[],
        summary: "a language server on stdio for editors, answering from these commands (CLI only)",
    },
];

const STEPS: (&str, &str, &str) = ("max-steps", "N", "stop each run after N statements and loop iterations");
const NATIVE: (&str, &str, &str) = ("native", "", "also build and run each program natively");
const INPUT: (&str, &str, &str) = ("input", "FILE", "main's stdin (default: empty)");

/// What main is given: its command-line arguments and its standard input.
#[derive(Clone, Debug, Default)]
pub struct ProgramInput {
    pub args: Vec<String>,
    /// A file, relative to the root, read as stdin.
    pub file: Option<String>,
    /// Without a file, read this process's own stdin (`anvil run` in a terminal); else it is empty.
    pub own_stdin: bool,
}

impl ProgramInput {
    fn given(&self) -> bool {
        !self.args.is_empty() || self.file.is_some()
    }

    /// The bytes of the input file, if there is one.
    fn file_bytes(&self, root: &Path) -> Result<Option<Vec<u8>>, Report> {
        match &self.file {
            None => Ok(None),
            Some(f) => std::fs::read(root.join(f))
                .map(Some)
                .map_err(|e| Report::usage(format!("cannot read --input {f}: {e}"))),
        }
    }
}

/// docs/SPEC.md, copied into this crate so it ships with the package (a test keeps them equal).
pub const SPEC: &str = include_str!("../SPEC.md");

/// `0.11.0 (x86_64-unknown-linux-gnu)`: the version and the target native builds produce.
pub fn version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), anvil_codegen::link::TARGET)
}

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

    /// A command that could not do its work (a C file that does not compile, say).
    pub(crate) fn error(message: String) -> Report {
        Report::failed(json!({ "ok": false, "error": message }), format!("error: {message}\n"), EXIT_FAILED)
    }
}

/// A source file named as the user gave it (diagnostics cite this name), read relative to `root`.
pub fn load(root: &Path, file: &str) -> Result<SourceFile, Report> {
    Files::disk(root).load(file)
}

/// Where source files come from: the disk under `root`, except files an editor has open, whose
/// unsaved text is read instead. `open` is keyed by file name relative to `root`, with `/`.
#[derive(Clone, Copy)]
pub struct Files<'a> {
    pub root: &'a Path,
    pub open: Option<&'a HashMap<String, String>>,
}

impl<'a> Files<'a> {
    pub fn disk(root: &'a Path) -> Files<'a> {
        Files { root, open: None }
    }

    pub fn read(&self, name: &str) -> Result<String, String> {
        let name = name.replace('\\', "/");
        // The standard library is in the binary, not on the disk.
        if let Some(text) = anvil_front::std_source(&name) {
            return Ok(text.to_string());
        }
        if let Some(text) = self.open.and_then(|o| o.get(&name)) {
            return Ok(text.clone());
        }
        std::fs::read_to_string(self.root.join(&name)).map_err(|e| e.to_string())
    }

    pub fn load(&self, file: &str) -> Result<SourceFile, Report> {
        match self.read(file) {
            Ok(text) => Ok(SourceFile::new(file.replace('\\', "/"), text)),
            Err(e) => Err(Report::usage(format!("cannot read {file}: {e}"))),
        }
    }

    /// Reads a program's modules, whose names are relative to `root` like the root file's.
    pub fn loader(self) -> impl Fn(&str) -> Result<String, String> + 'a {
        move |name: &str| self.read(name)
    }
}

fn pos(src: &SourceFile, offset: usize) -> Value {
    let (line, col) = src.line_col(offset);
    json!({ "line": line, "col": col })
}

/// Reads a program's modules from under `root`, where every file name is relative to it.
pub(crate) fn loader(root: &Path) -> impl Fn(&str) -> Result<String, String> + '_ {
    Files::disk(root).loader()
}

/// How many errors saying the same thing are written out; `--json` has them all.
const SAME_ERRORS: usize = 2;

/// Errors as text: one cause can give dozens that say the same thing, so past the first of them
/// only their count.
fn rendered(sources: &[SourceFile], diags: &[Diagnostic]) -> String {
    let mut text = String::new();
    let mut said: Vec<(&str, &str, usize)> = Vec::new();
    for d in diags {
        let at = match said.iter().position(|(c, m, _)| *c == d.code && *m == d.message) {
            Some(at) => at,
            None => {
                said.push((d.code, d.message.as_str(), 0));
                said.len() - 1
            }
        };
        said[at].2 += 1;
        if said[at].2 <= SAME_ERRORS {
            text.push_str(&diag::render(&sources[d.file], d));
            text.push('\n');
        }
    }
    for (code, message, n) in said.iter().filter(|(_, _, n)| *n > SAME_ERRORS) {
        text.push_str(&format!("... and {} more error[{code}]: {message}\n", n - SAME_ERRORS));
    }
    text
}

/// Compile errors as a failed report; `sources` are the program's files, which each
/// diagnostic's `file` indexes.
pub(crate) fn diagnostics(sources: &[SourceFile], diags: &[Diagnostic]) -> Report {
    let ds: Vec<_> = diags.iter().map(|d| diag::to_json(&sources[d.file], d)).collect();
    let mut text = rendered(sources, diags);
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

/// `compile`, keeping the lines of statements for `profile`.
fn compile_lines(root: &Path, file: &str) -> Result<Program, Report> {
    let src = load(root, file)?;
    match anvil_front::compile_lines(&src, &loader(root)) {
        (_, Ok(p)) => Ok(p),
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
    check_in(Files::disk(root), file)
}

pub fn check_in(files: Files, file: &str) -> Report {
    let src = try_report!(files.load(file));
    match anvil_front::compile_with(&src, &files.loader()) {
        (_, Ok(_)) => Report::ok(json!({ "ok": true, "diagnostics": [] }), String::new()),
        (sources, Err(d)) => diagnostics(&sources, &d),
    }
}

/// Interprets main, writing its output to `out` as it runs. The report's JSON holds the exit
/// code and, if it stopped early, the panic or the step limit. Files are real, relative to `root`.
pub fn run(
    root: &Path,
    file: &str,
    max_steps: Option<u64>,
    input: &ProgramInput,
    (out, err): (&mut (dyn Write + Send), &mut (dyn Write + Send)),
) -> Report {
    let (_, program) = try_report!(with_main(root, file));
    let bytes = try_report!(input.file_bytes(root));
    let mut ffi = match c_functions(&program) {
        Ok(f) => f,
        Err(e) => return Report::failed(json!({ "ok": false, "error": e }), format!("error: {e}\n"), EXIT_FAILED),
    };
    // A thread with a stack big enough for MAX_CALL_DEPTH interpreted calls; output streams.
    let outcome = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(s, || {
                let mut stdin: Box<dyn BufRead> = match (&bytes, input.own_stdin) {
                    (Some(b), _) => Box::new(std::io::Cursor::new(b.as_slice())),
                    (None, true) => Box::new(std::io::BufReader::new(std::io::stdin())),
                    (None, false) => Box::new(std::io::empty()),
                };
                let files = anvil_interp::Files { root, memory: false };
                let host = host(anvil_interp::Clock::Real, &mut ffi, &input.args, Some(&mut *stdin), files, err);
                let host = anvil_interp::Host { env: anvil_interp::Env::Real, ..host };
                anvil_interp::run_main(&program, out, max_steps, host)
            })
            .expect("cannot spawn interpreter thread")
            .join()
            .expect("interpreter thread crashed")
    });
    let _ = out.flush();
    let _ = err.flush();
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
        None => root.join(file).with_extension(std::env::consts::EXE_EXTENSION),
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

/// The C functions of a program's extern declarations, built and loaded for the interpreter.
pub(crate) struct Ffi(anvil_codegen::ffi::CLibrary);

impl anvil_interp::Foreign for Ffi {
    fn call(&mut self, func: usize, args: &[u64]) -> u64 {
        self.0.call(func, args)
    }
}

/// Builds the library of `program`'s C functions; None when it declares no externs.
pub(crate) fn c_functions(program: &Program) -> Result<Option<Ffi>, String> {
    let dir = std::env::temp_dir().join(format!("anvil_ffi_{}_{}", std::process::id(), unique()));
    Ok(anvil_codegen::ffi::load(program, &dir)?.map(Ffi))
}

/// The host of every checked run (tests, goldens, traces, profiles): the virtual clock, files
/// read from under `root` with writes kept in memory, and the program's C functions.
/// `env` is the whole environment the run sees.
pub(crate) fn checked_host<'h>(
    ffi: &'h mut Option<Ffi>,
    root: &'h Path,
    args: &'h [String],
    stdin: Option<&'h mut dyn BufRead>,
    stderr: &'h mut dyn Write,
    env: &'h [(String, String)],
) -> anvil_interp::Host<'h> {
    let host = host(anvil_interp::Clock::Virtual, ffi, args, stdin, anvil_interp::Files { root, memory: true }, stderr);
    anvil_interp::Host { env: anvil_interp::Env::Given(env), ..host }
}

/// A host whose environment is empty; `run` gives it the process's own.
pub(crate) fn host<'h>(
    clock: anvil_interp::Clock,
    ffi: &'h mut Option<Ffi>,
    args: &'h [String],
    stdin: Option<&'h mut dyn BufRead>,
    files: anvil_interp::Files<'h>,
    stderr: &'h mut dyn Write,
) -> anvil_interp::Host<'h> {
    let foreign = ffi.as_mut().map(|f| f as &mut dyn anvil_interp::Foreign);
    anvil_interp::Host { clock, foreign, args, stdin, files, stderr, env: anvil_interp::Env::Given(&[]) }
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
            text.push_str(&rendered(&sources, &a.diags));
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
pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn outline(root: &Path, file: &str) -> Report {
    outline_in(Files::disk(root), file)
}

pub fn outline_in(files: Files, file: &str) -> Report {
    use anvil_front::ast::Item;
    let src = try_report!(files.load(file));
    let ast = match anvil_front::parse(&src) {
        Ok(f) => f,
        Err(d) => return diagnostics(std::slice::from_ref(&src), &d),
    };
    let text = |s: Span| src.slice(s).to_string();
    let line = |s: Span| src.line_col(s.start).0;
    let doc = |name: Span| anvil_front::doc::doc_above(&src.text, name.start);
    let mut items = Vec::new();
    let mut human = String::new();
    for item in &ast.items {
        let named = match item {
            Item::Fn(f) => Some(f.name.span),
            Item::Struct(s) => Some(s.name.span),
            Item::Enum(e) => Some(e.name.span),
            Item::Interface(i) => Some(i.name.span),
            Item::Unit(u) => Some(u.name.span),
            Item::Const(c) => Some(c.name.span),
            Item::Import(_) | Item::Test(_) => None,
        };
        // The doc's first line, in the text; all of it in the JSON.
        let doc = named.and_then(doc);
        if let Some(d) = &doc {
            human.push_str(&format!("      // {}\n", d.lines().next().unwrap_or("")));
        }
        let mut v = match item {
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
                let name = match &f.receiver {
                    Some(r) => format!("{}.{}", r.ty.name, f.name.name),
                    None => f.name.name.clone(),
                };
                json!({
                    "kind": if f.receiver.is_some() { "method" } else { "fn" },
                    "name": name, "pub": f.public, "line": line(f.span), "signature": sig,
                    "effects": f.uses.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
                    "noalloc": f.noalloc.is_some(),
                    "extern": f.external.as_ref().map(|x| match &x.source {
                        Some((w, _)) => json!({ "file": anvil_front::c_file(&src.name, w) }),
                        None => json!({ "file": null }),
                    }),
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
                json!({ "kind": "struct", "name": s.name.name, "pub": s.public, "line": line(s.span), "fields": fields })
            }
            Item::Enum(e) => {
                let variants: Vec<String> = e.variants.iter().map(|v| one_line(&text(v.span))).collect();
                human.push_str(&format!("{:>4}  enum {} {{ {} }}\n", line(e.span), e.name.name, variants.join(", ")));
                json!({ "kind": "enum", "name": e.name.name, "pub": e.public, "line": line(e.span), "variants": variants })
            }
            Item::Interface(i) => {
                let methods: Vec<String> =
                    i.methods.iter().map(|m| one_line(&text(m.span)).trim_end_matches(';').to_string()).collect();
                human.push_str(&format!(
                    "{:>4}  interface {} {{ {} }}\n",
                    line(i.span),
                    i.name.name,
                    methods.join("; ")
                ));
                json!({ "kind": "interface", "name": i.name.name, "pub": i.public, "line": line(i.span), "methods": methods })
            }
            Item::Unit(u) => {
                human.push_str(&format!("{:>4}  {}\n", line(u.span), text(u.span)));
                json!({ "kind": "unit", "name": u.name.name, "pub": u.public, "line": line(u.span), "text": text(u.span) })
            }
            Item::Const(c) => {
                human.push_str(&format!("{:>4}  {}\n", line(c.span), text(c.span)));
                json!({ "kind": "const", "name": c.name.name, "pub": c.public, "line": line(c.span), "text": text(c.span) })
            }
            Item::Test(t) => {
                human.push_str(&format!("{:>4}  test \"{}\"\n", line(t.span), t.name));
                json!({ "kind": "test", "name": t.name, "line": line(t.span) })
            }
        };
        if named.is_some() {
            v["doc"] = json!(doc);
        }
        items.push(v);
    }
    Report::ok(json!({ "ok": true, "file": src.name, "items": items }), human)
}

pub fn type_at(root: &Path, file: &str, line: usize, col: usize) -> Report {
    type_at_in(Files::disk(root), file, line, col)
}

pub fn refs(root: &Path, file: &str, name: &str) -> Report {
    refs_in(Files::disk(root), file, name)
}

pub fn complete(root: &Path, file: &str, line: usize, col: usize) -> Report {
    complete_in(Files::disk(root), file, line, col)
}

/// The file in canonical layout, without writing it: `{ok, text}`, or the error that stops
/// formatting.
pub fn formatted_in(files: Files, file: &str) -> Report {
    let src = try_report!(files.load(file));
    match anvil_front::fmt::format(&src) {
        Ok(text) => Report::ok(json!({ "ok": true, "changed": text != src.text, "text": text }), String::new()),
        Err(d) => diagnostics(std::slice::from_ref(&src), &[d]),
    }
}

/// The standard library: every module with its pub items, or one module with its source too.
pub fn std_modules(module: Option<&str>) -> Report {
    let names: Vec<&str> = anvil_front::STD_MODULES.iter().map(|(n, _)| *n).collect();
    let wanted: Vec<&str> = match module {
        None => names.clone(),
        Some(m) => {
            let m = m.strip_prefix("std.").unwrap_or(m);
            if !names.contains(&m) {
                return Report::usage(format!("the standard library has no module `{m}`; it has {}", names.join(", ")));
            }
            vec![m]
        }
    };
    let mut modules = Vec::new();
    let mut text = String::new();
    for m in wanted {
        let file = format!("std/{m}.anv");
        let outline = outline_in(Files::disk(Path::new(".")), &file);
        let items: Vec<Value> = outline.json["items"]
            .as_array()
            .map(|a| a.iter().filter(|i| i["pub"] == true).cloned().collect())
            .unwrap_or_default();
        text.push_str(&format!(
            "import std.{m}.{{...}}
"
        ));
        for i in &items {
            let shown = match i["signature"].as_str() {
                Some(sig) => sig.to_string(),
                None => format!("pub {} {}", i["kind"].as_str().unwrap_or(""), i["name"].as_str().unwrap_or("")),
            };
            text.push_str(&format!(
                "    {shown}
"
            ));
            for l in i["doc"].as_str().into_iter().flat_map(str::lines) {
                text.push_str(&format!("        // {l}\n"));
            }
        }
        let mut v = json!({ "module": format!("std.{m}"), "file": file, "items": items });
        if module.is_some() {
            let source = anvil_front::std_source(&file).unwrap_or_default();
            v["source"] = json!(source);
            text.push('\n');
            text.push_str(source);
        }
        modules.push(v);
    }
    Report::ok(json!({ "ok": true, "modules": modules }), text)
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
