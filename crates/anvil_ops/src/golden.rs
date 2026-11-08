//! Approved program output. `<golden dir>/<file>.out` holds what a program printed, its exit
//! code and any panic. `golden check` runs every project file that has a main and compares: on
//! the interpreter, and with `native` also as an executable, which must free every object it
//! made. Only `golden approve` writes a golden, one file at a time, after a person or agent has
//! looked at the output. When a program differs, its actual output and a line diff are written
//! to `out/golden/<file>.out` and `.diff` (`.native.out` and `.native.diff` for the executable).
//!
//! ```text
//! exit: 101
//! panic: index 3 out of bounds for length 3 at src/main.anv:7:22
//! panic: (each further line of a panic's text says `panic:` too)
//! stderr: a line the program wrote with eprint
//! ---
//! everything the program printed, byte for byte
//! ```
//!
//! A program that reads its arguments or stdin is given them by `<golden dir>/<file>.in`, which a
//! person writes; without one it runs with neither. Runs read files under the project root and
//! keep their writes in memory.
//!
//! ```text
//! arg: --verbose
//! arg: two words
//! env: HOME=/home/ada
//! ---
//! the program's stdin, byte for byte
//! ```

use crate::project::Project;
use anvil_front::ir::{Program, PANIC_EXIT_CODE};
use anvil_front::span::SourceFile;
use anvil_interp::{FuncStats, Recorder};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a golden run is given: its arguments and its stdin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Input {
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    /// The environment the run sees: nothing else.
    pub env: Vec<(String, String)>,
}

impl Input {
    pub fn parse(bytes: &[u8]) -> Result<Input, String> {
        let mut args = Vec::new();
        let mut env = Vec::new();
        let mut rest = bytes;
        loop {
            let end =
                rest.iter().position(|&b| b == b'\n').ok_or("no `---` line separating the arguments from the input")?;
            let line = std::str::from_utf8(&rest[..end]).map_err(|_| "a line before `---` is not UTF-8")?;
            rest = &rest[end + 1..];
            if line == "---" {
                return Ok(Input { args, stdin: rest.to_vec(), env });
            }
            if let Some(a) = line.strip_prefix("arg: ") {
                args.push(a.to_string());
            } else if let Some(kv) = line.strip_prefix("env: ") {
                let Some((k, v)) = kv.split_once('=').filter(|(k, _)| !k.is_empty()) else {
                    return Err(format!("`{line}` is not `env: NAME=value`"));
                };
                env.push((k.to_string(), v.to_string()));
            } else {
                return Err(format!("unknown line `{line}`; expected `arg: `, `env: ` or `---`"));
            }
        }
    }
}

/// The files of the golden directory that belong to no project file, each with what is wrong.
/// A wrongly named input is otherwise a program quietly run with none.
pub fn orphans(project: &Project) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let dir = project.root.join(&project.golden_dir);
    let mut found = Vec::new();
    walk(&dir, &mut found);
    found.sort();
    let mut out = Vec::new();
    for p in found {
        let Ok(rel) = p.strip_prefix(&dir) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let Some((base, kind)) = rel.rsplit_once('.') else { continue };
        if !matches!(kind, "out" | "in" | "usage" | "api") || project.files.iter().any(|f| f == base) {
            continue;
        }
        let shown = format!("{}/{rel}", project.golden_dir);
        // The file it was meant for: `main` or `main.anv`, wherever in the project that is.
        let name = base.rsplit('/').next().unwrap_or(base);
        let name = if name.ends_with(".anv") { name.to_string() } else { format!("{name}.anv") };
        let meant = project.files.iter().find(|f| f.rsplit('/').next() == Some(name.as_str()));
        out.push(match meant {
            Some(source) => format!("{shown} is not read: for {source} it would be {}/{source}.{kind}", project.golden_dir),
            None => format!("{shown} is not read: no project file is named {base}"),
        });
    }
    out
}

/// Where a program's input lives: `<golden dir>/<file>.in`.
pub fn input_path(project: &Project, file: &str) -> PathBuf {
    project.root.join(&project.golden_dir).join(format!("{file}.in"))
}

/// The program's input, or none when it has no `.in` file.
pub fn read_input(project: &Project, file: &str) -> Result<Input, String> {
    let p = input_path(project, file);
    match std::fs::read(&p) {
        Ok(b) => Input::parse(&b).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Input::default()),
        Err(e) => Err(format!("cannot read {}: {e}", p.display())),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub exit: i32,
    pub panic: Option<String>,
    pub stdout: String,
    /// What eprint wrote (the panic message is apart).
    pub stderr: String,
}

impl Output {
    pub fn to_text(&self) -> String {
        let mut s = format!("exit: {}\n", self.exit);
        if let Some(p) = &self.panic {
            for line in p.split('\n') {
                s.push_str(&format!("panic: {line}\n"));
            }
        }
        for line in self.stderr.lines() {
            s.push_str(&format!("stderr: {line}\n"));
        }
        s.push_str("---\n");
        s.push_str(&self.stdout);
        s
    }

    pub fn parse(text: &str) -> Result<Output, String> {
        let (head, stdout) = text.split_once("---\n").ok_or("no `---` line separating the header from the output")?;
        let mut exit = None;
        let mut panic = None;
        let mut stderr = String::new();
        for line in head.lines() {
            if let Some(v) = line.strip_prefix("exit: ") {
                exit = Some(v.parse().map_err(|_| format!("bad exit code `{v}`"))?);
            } else if let Some(v) = line.strip_prefix("panic: ") {
                panic = Some(match panic {
                    Some(before) => format!("{before}\n{v}"),
                    None => v.to_string(),
                });
            } else if let Some(v) = line.strip_prefix("stderr: ") {
                stderr.push_str(v);
                stderr.push('\n');
            } else {
                return Err(format!("unknown header line `{line}`; expected `exit:`, `panic:` or `stderr:`"));
            }
        }
        Ok(Output { exit: exit.ok_or("no `exit:` line")?, panic, stdout: stdout.to_string(), stderr })
    }

    pub fn to_json(&self) -> Value {
        json!({ "exit": self.exit, "panic": self.panic, "stdout": self.stdout, "stderr": self.stderr })
    }
}

/// How `got` differs from `want`, or None when they are the same.
pub fn difference(want: &Output, got: &Output) -> Option<String> {
    if want.panic != got.panic {
        return Some(match (&want.panic, &got.panic) {
            (Some(w), Some(g)) => format!("panics with `{g}`, approved `{w}`"),
            (None, Some(g)) => format!("panics with `{g}`; the approved run did not panic"),
            (Some(w), None) => format!("does not panic; the approved run panicked with `{w}`"),
            (None, None) => unreachable!(),
        });
    }
    if want.exit != got.exit {
        return Some(format!("exits with {}, approved {}", got.exit, want.exit));
    }
    lines_differ("output", &want.stdout, &got.stdout).or_else(|| lines_differ("stderr", &want.stderr, &got.stderr))
}

/// The first line where `got` differs from `want`.
fn lines_differ(what: &str, want: &str, got: &str) -> Option<String> {
    if want == got {
        return None;
    }
    let (w, g): (Vec<&str>, Vec<&str>) = (want.split('\n').collect(), got.split('\n').collect());
    for i in 0..w.len().max(g.len()) {
        match (w.get(i), g.get(i)) {
            (Some(a), Some(b)) if a == b => continue,
            (Some(a), Some(b)) => return Some(format!("{what} line {}: printed `{b}`, approved `{a}`", i + 1)),
            (Some(a), None) => return Some(format!("{what} stops before line {}; approved `{a}`", i + 1)),
            (None, Some(b)) => return Some(format!("{what} has an extra line {}: `{b}`", i + 1)),
            (None, None) => unreachable!(),
        }
    }
    None
}

/// What one interpreted run cost: its steps and, per function, calls and steps.
#[derive(Clone, Debug)]
pub struct Counts {
    pub steps: u64,
    /// Bytes allocated and bytes copied, by the interpreter's model.
    pub bytes: u64,
    pub copied: u64,
    pub funcs: Vec<FuncStats>,
    /// Runs of each branch, by BranchId.
    pub branches: Vec<u64>,
}

impl Counts {
    pub fn of(rec: &Recorder) -> Counts {
        Counts {
            steps: rec.steps,
            bytes: rec.bytes,
            copied: rec.copied,
            funcs: rec.funcs.clone(),
            branches: rec.branches.clone(),
        }
    }
}

/// Runs main in the interpreter, given `input`, with files under `root`; Err when the step limit
/// stops it.
pub fn interpret(
    program: Program,
    max_steps: Option<u64>,
    root: &Path,
    input: &Input,
) -> Result<(Output, Counts), String> {
    interpret_reaching(program, max_steps, root, input).map(|(out, counts, _)| (out, counts))
}

/// The lines of statements a run ran (`file:line`), for a program compiled with its lines.
pub(crate) fn lines_run(program: &Program, rec: &Recorder) -> HashSet<String> {
    program.lines.iter().zip(&rec.lines).filter(|(_, n)| n[0] > 0).map(|(at, _)| at.clone()).collect()
}

/// `interpret`, and the lines the run ran.
pub(crate) fn interpret_reaching(
    program: Program,
    max_steps: Option<u64>,
    root: &Path,
    input: &Input,
) -> Result<(Output, Counts, HashSet<String>), String> {
    let mut ffi = crate::c_functions(&program)?;
    let (root, input) = (root.to_path_buf(), input.clone());
    let (outcome, bytes, err, counts, entered) = anvil_interp::run_on_big_stack(move || {
        let mut buf = Vec::new();
        let mut err = Vec::new();
        let mut rec = Recorder::new(&program);
        let mut stdin = std::io::Cursor::new(input.stdin.as_slice());
        let host = crate::checked_host(&mut ffi, &root, &input.args, Some(&mut stdin), &mut err, &input.env);
        let o = anvil_interp::run_main_recorded(&program, &mut buf, max_steps, Some(&mut rec), host);
        (o, buf, err, Counts::of(&rec), lines_run(&program, &rec))
    });
    let stdout = String::from_utf8_lossy(&bytes).into_owned();
    let stderr = String::from_utf8_lossy(&err).into_owned();
    match outcome {
        anvil_interp::Outcome::Exit(exit) => Ok((Output { exit, panic: None, stdout, stderr }, counts, entered)),
        anvil_interp::Outcome::Panicked(m) => {
            Ok((Output { exit: PANIC_EXIT_CODE, panic: Some(m), stdout, stderr }, counts, entered))
        }
        anvil_interp::Outcome::OutOfSteps(n) => Err(anvil_interp::step_limit_message(n)),
    }
}

/// A line diff of two texts: `  ` in both, `- ` only in `want`, `+ ` only in `got`. Long runs of
/// equal lines are cut to two lines of context on each side.
pub fn diff_lines(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.split_inclusive('\n').collect();
    let g: Vec<&str> = got.split_inclusive('\n').collect();
    let mut ops: Vec<(char, &str)> = Vec::new();
    if w.len().saturating_mul(g.len()) > 4_000_000 {
        // Too long for the table below: show both sides whole.
        ops.extend(w.iter().map(|l| ('-', *l)));
        ops.extend(g.iter().map(|l| ('+', *l)));
    } else {
        // lcs[i][j]: longest common subsequence of w[i..] and g[j..].
        let mut lcs = vec![vec![0u32; g.len() + 1]; w.len() + 1];
        for i in (0..w.len()).rev() {
            for j in (0..g.len()).rev() {
                lcs[i][j] = if w[i] == g[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < w.len() || j < g.len() {
            if i < w.len() && j < g.len() && w[i] == g[j] {
                ops.push((' ', w[i]));
                i += 1;
                j += 1;
            } else if i < w.len() && (j == g.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
                // What was approved goes first, then what was printed instead.
                ops.push(('-', w[i]));
                i += 1;
            } else {
                ops.push(('+', g[j]));
                j += 1;
            }
        }
    }
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, (k, _))| *k != ' ').map(|(i, _)| i).collect();
    let near = |i: usize| changed.iter().any(|&c| c.abs_diff(i) <= 2);
    let mut out = String::new();
    let mut skipped = false;
    for (i, (kind, line)) in ops.iter().enumerate() {
        if !near(i) {
            skipped = true;
            continue;
        }
        if skipped {
            out.push_str("...\n");
            skipped = false;
        }
        out.push(*kind);
        out.push(' ');
        out.push_str(line);
        if !line.ends_with('\n') {
            out.push_str("\n(no newline at the end)\n");
        }
    }
    if skipped {
        out.push_str("...\n");
    }
    out
}

fn out_dir(project: &Project) -> PathBuf {
    project.root.join("out").join("golden")
}

/// Writes the actual output and its diff against the golden; returns where, for the message.
fn write_actual(project: &Project, file: &str, suffix: &str, want: &Output, got: &Output) -> String {
    let out = out_dir(project).join(format!("{file}{suffix}.out"));
    let diff = out_dir(project).join(format!("{file}{suffix}.diff"));
    let written = out.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| {
        std::fs::write(&out, got.to_text())?;
        std::fs::write(&diff, diff_lines(&want.to_text(), &got.to_text()))
    });
    match written {
        Ok(()) => format!("; actual output in {}, diff in {}", rel(project, &out), rel(project, &diff)),
        Err(e) => format!("; could not write {}: {e}", out.display()),
    }
}

fn rel(project: &Project, p: &Path) -> String {
    p.strip_prefix(&project.root).unwrap_or(p).display().to_string().replace('\\', "/")
}

/// Removes what `write_actual` left for a file, so out/golden never shows a stale failure.
pub fn clear_actual(project: &Project, file: &str) {
    for suffix in [".out", ".diff", ".native.out", ".native.diff", ".release.out", ".release.diff"] {
        let _ = std::fs::remove_file(out_dir(project).join(format!("{file}{suffix}")));
    }
}

/// Builds and runs main natively with the leak check on, given `input`, in `root`; Err for a
/// build failure or a runtime error (a leak or a refcount fault, which are bugs in Anvil, not in
/// the program).
pub fn native(program: &Program, scratch: &Path, name: &str, root: &Path, input: &Input) -> Result<Output, String> {
    native_as(program, scratch, name, root, input, false)
}

/// Like `native`, built by the release backend (through C) when `release`.
pub fn native_as(
    program: &Program,
    scratch: &Path,
    name: &str,
    root: &Path,
    input: &Input,
    release: bool,
) -> Result<Output, String> {
    std::fs::create_dir_all(scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let exe = scratch.join(format!("{}{}", name.replace(['/', '\\', '.'], "_"), std::env::consts::EXE_SUFFIX));
    if release {
        anvil_codegen::build_exe_release(program, &exe, &scratch.join("work"))?;
    } else {
        anvil_codegen::build_exe(program, &exe, &scratch.join("work"), false)?;
    }
    let args: Vec<&str> = input.args.iter().map(String::as_str).collect();
    let result = run_native(&exe, &args, root, &input.stdin, &input.env);
    let _ = std::fs::remove_file(&exe);
    result
}

/// Runs a built executable as a checked run (the leak check on, the virtual clock, writes kept in
/// memory) in `cwd` with `stdin`, and reads what it did. Err for a runtime error (a leak or
/// refcount fault: a bug in Anvil, not in the program) or odd stderr.
pub(crate) fn run_native(
    exe: &Path,
    args: &[&str],
    cwd: &Path,
    stdin: &[u8],
    env: &[(String, String)],
) -> Result<Output, String> {
    let exe = std::path::absolute(exe).map_err(|e| format!("cannot find {}: {e}", exe.display()))?;
    // The environment is what the run is given and nothing else, as in the interpreter.
    let mut child = Command::new(&exe)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .env("ANVIL_CHECK_LEAKS", "1")
        .env("ANVIL_CLOCK", "virtual")
        .env("ANVIL_FS", "memory")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    // Written from a thread, so a program that prints before it reads cannot block on a full pipe.
    let mut pipe = child.stdin.take().expect("stdin is piped");
    let bytes = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        // A program may exit without reading it all; that is not an error.
        let _ = pipe.write_all(&bytes);
    });
    let out = child.wait_with_output().map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let _ = writer.join();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let exit = out.status.code().ok_or("the executable was killed")?;
    if let Some(at) = stderr.find("anvil runtime error: ") {
        return Err(format!("runtime error: {}", stderr[at + 21..].trim_end()));
    }
    // A panic is the last line of stderr; what eprint wrote comes before it.
    let mut err = stderr.into_owned();
    let mut panic = None;
    if exit == PANIC_EXIT_CODE {
        let body = err.strip_suffix('\n').unwrap_or(&err);
        // A panic's text can run over several lines, and comes last.
        let start = body.rfind("\npanic: ").map(|i| i + 1).or(body.starts_with("panic: ").then_some(0));
        if let Some(start) = start {
            panic = Some(body[start + "panic: ".len()..].to_string());
            err.truncate(start);
        }
    }
    Ok(Output { exit, panic, stdout, stderr: err })
}

pub fn path(project: &Project, file: &str) -> PathBuf {
    project.root.join(&project.golden_dir).join(format!("{file}.out"))
}

pub fn read(project: &Project, file: &str) -> Result<Option<Output>, String> {
    let p = path(project, file);
    match std::fs::read_to_string(&p) {
        // Byte for byte: .gitattributes keeps git from rewriting line endings in goldens.
        Ok(t) => Output::parse(&t).map(Some).map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", p.display())),
    }
}

pub fn write(project: &Project, file: &str, out: &Output) -> Result<PathBuf, String> {
    let p = path(project, file);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(&p, out.to_text()).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    Ok(p)
}

/// One file's golden result: `status` is ok, no_main, missing, differs, stopped or error.
pub struct Checked {
    pub file: String,
    pub status: &'static str,
    pub detail: Option<String>,
    /// The interpreted run of main, when there was one.
    pub counts: Option<Counts>,
}

impl Checked {
    pub fn failed(&self) -> bool {
        !matches!(self.status, "ok" | "no_main")
    }
    pub fn to_json(&self) -> Value {
        let steps = self.counts.as_ref().map(|c| c.steps);
        json!({ "file": self.file, "status": self.status, "detail": self.detail, "steps": steps })
    }
}

/// Checks one compiled project file against its golden.
pub fn check_file(
    project: &Project,
    src: &SourceFile,
    program: Program,
    native_too: bool,
    release: bool,
    max_steps: Option<u64>,
) -> Checked {
    let file = src.name.clone();
    let done = |status, detail: Option<String>| Checked { file: file.clone(), status, detail, counts: None };
    if program.main.is_none() {
        return done("no_main", None);
    }
    let want = match read(project, &file) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return done(
                "missing",
                Some(format!("no approved output; run it, check what it prints, then `anvil golden approve {file}`")),
            )
        }
        Err(e) => return done("error", Some(e)),
    };
    let input = match read_input(project, &file) {
        Ok(i) => i,
        Err(e) => return done("error", Some(e)),
    };
    let native_program = (native_too || release).then(|| program.clone());
    let (got, counts) = match interpret(program, max_steps, &project.root, &input) {
        Ok(o) => o,
        Err(m) => return done("stopped", Some(m)),
    };
    let with = |status, detail: Option<String>| Checked { counts: Some(counts.clone()), ..done(status, detail) };
    clear_actual(project, &file);
    if let Some(d) = difference(&want, &got) {
        let written = write_actual(project, &file, "", &want, &got);
        return with("differs", Some(format!("interpreter {d}{written}")));
    }
    if let Some(p) = native_program {
        let backends: &[(bool, &str)] = if release { &[(false, "native"), (true, "release")] } else { &[(false, "native")] };
        for (build_release, name) in backends {
            let scratch = std::env::temp_dir().join(format!("anvil_golden_{}_{}", std::process::id(), crate::unique()));
            let result = native_as(&p, &scratch, &file, &project.root, &input, *build_release);
            let _ = std::fs::remove_dir_all(&scratch);
            match result {
                Ok(n) => {
                    if let Some(d) = difference(&want, &n) {
                        let written = write_actual(project, &file, &format!(".{name}"), &want, &n);
                        return with("differs", Some(format!("{name} {d}{written}")));
                    }
                }
                Err(e) => return with("error", Some(format!("{name}: {e}"))),
            }
        }
    }
    with("ok", None)
}
