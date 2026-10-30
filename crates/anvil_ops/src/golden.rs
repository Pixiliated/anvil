//! Approved program output. `<golden dir>/<file>.out` holds what a program printed, its exit
//! code and any panic. `golden check` runs every project file that has a main and compares: on
//! the interpreter, and with `native` also as an executable, which must free every object it
//! made. Only `golden approve` writes a golden, one file at a time, after a person or agent has
//! looked at the output.
//!
//! ```text
//! exit: 101
//! panic: index 3 out of bounds for length 3 at src/main.anv:7:22
//! ---
//! everything the program printed, byte for byte
//! ```

use crate::project::Project;
use anvil_front::ir::{Program, PANIC_EXIT_CODE};
use anvil_front::span::SourceFile;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub exit: i32,
    pub panic: Option<String>,
    pub stdout: String,
}

impl Output {
    pub fn to_text(&self) -> String {
        let mut s = format!("exit: {}\n", self.exit);
        if let Some(p) = &self.panic {
            s.push_str(&format!("panic: {p}\n"));
        }
        s.push_str("---\n");
        s.push_str(&self.stdout);
        s
    }

    pub fn parse(text: &str) -> Result<Output, String> {
        let (head, stdout) = text.split_once("---\n").ok_or("no `---` line separating the header from the output")?;
        let mut exit = None;
        let mut panic = None;
        for line in head.lines() {
            if let Some(v) = line.strip_prefix("exit: ") {
                exit = Some(v.parse().map_err(|_| format!("bad exit code `{v}`"))?);
            } else if let Some(v) = line.strip_prefix("panic: ") {
                panic = Some(v.to_string());
            } else {
                return Err(format!("unknown header line `{line}`; expected `exit:` or `panic:`"));
            }
        }
        Ok(Output { exit: exit.ok_or("no `exit:` line")?, panic, stdout: stdout.to_string() })
    }

    pub fn to_json(&self) -> Value {
        json!({ "exit": self.exit, "panic": self.panic, "stdout": self.stdout })
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
    if want.stdout != got.stdout {
        let (w, g): (Vec<&str>, Vec<&str>) = (want.stdout.split('\n').collect(), got.stdout.split('\n').collect());
        for i in 0..w.len().max(g.len()) {
            match (w.get(i), g.get(i)) {
                (Some(a), Some(b)) if a == b => continue,
                (Some(a), Some(b)) => return Some(format!("output line {}: printed `{b}`, approved `{a}`", i + 1)),
                (Some(a), None) => return Some(format!("output stops before line {}; approved `{a}`", i + 1)),
                (None, Some(b)) => return Some(format!("output has an extra line {}: `{b}`", i + 1)),
                (None, None) => unreachable!(),
            }
        }
    }
    None
}

/// Runs main in the interpreter; Err when the step limit stops it.
pub fn interpret(program: Program, max_steps: Option<u64>) -> Result<Output, String> {
    let (outcome, bytes) = anvil_interp::run_on_big_stack(move || {
        let mut buf = Vec::new();
        let o = anvil_interp::run_main(&program, &mut buf, max_steps);
        (o, buf)
    });
    let stdout = String::from_utf8_lossy(&bytes).into_owned();
    match outcome {
        anvil_interp::Outcome::Exit(exit) => Ok(Output { exit, panic: None, stdout }),
        anvil_interp::Outcome::Panicked(m) => Ok(Output { exit: PANIC_EXIT_CODE, panic: Some(m), stdout }),
        anvil_interp::Outcome::OutOfSteps(n) => Err(anvil_interp::step_limit_message(n)),
    }
}

/// Builds and runs main natively with the leak check on; Err for a build failure or a runtime
/// error (a leak or a refcount fault, which are bugs in Anvil, not in the program).
pub fn native(program: &Program, scratch: &Path, name: &str) -> Result<Output, String> {
    std::fs::create_dir_all(scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let exe = scratch.join(format!("{}.exe", name.replace(['/', '\\', '.'], "_")));
    anvil_codegen::build_exe(program, &exe, &scratch.join("work"), false)?;
    let out = Command::new(&exe)
        .env("ANVIL_CHECK_LEAKS", "1")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let _ = std::fs::remove_file(&exe);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let exit = out.status.code().ok_or("the executable was killed")?;
    if let Some(m) = stderr.strip_prefix("anvil runtime error: ") {
        return Err(format!("runtime error: {}", m.trim_end()));
    }
    let panic = match stderr.strip_prefix("panic: ") {
        Some(m) => Some(m.trim_end_matches('\n').to_string()),
        None if stderr.is_empty() => None,
        None => return Err(format!("unexpected stderr: {stderr}")),
    };
    Ok(Output { exit, panic, stdout })
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
}

impl Checked {
    pub fn failed(&self) -> bool {
        !matches!(self.status, "ok" | "no_main")
    }
    pub fn to_json(&self) -> Value {
        json!({ "file": self.file, "status": self.status, "detail": self.detail })
    }
}

/// Checks one compiled project file against its golden.
pub fn check_file(
    project: &Project,
    src: &SourceFile,
    program: Program,
    native_too: bool,
    max_steps: Option<u64>,
) -> Checked {
    let file = src.name.clone();
    let done = |status, detail: Option<String>| Checked { file: file.clone(), status, detail };
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
    let native_program = native_too.then(|| program.clone());
    let got = match interpret(program, max_steps) {
        Ok(o) => o,
        Err(m) => return done("stopped", Some(m)),
    };
    if let Some(d) = difference(&want, &got) {
        return done("differs", Some(format!("interpreter {d}")));
    }
    if let Some(p) = native_program {
        let scratch = std::env::temp_dir().join(format!("anvil_golden_{}_{}", std::process::id(), crate::unique()));
        let result = native(&p, &scratch, &file);
        let _ = std::fs::remove_dir_all(&scratch);
        match result {
            Ok(n) => {
                if let Some(d) = difference(&want, &n) {
                    return done("differs", Some(format!("native {d}")));
                }
            }
            Err(e) => return done("error", Some(format!("native: {e}"))),
        }
    }
    done("ok", None)
}
