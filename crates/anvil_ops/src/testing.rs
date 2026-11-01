//! `anvil test`: every test block in the interpreter, property tests (`for all`) on generated
//! inputs, and with `native` the plain tests again as one executable, whose results must equal
//! the interpreter's (pass or the same panic, the same output, every object freed).

use crate::{compile, golden, unique, Report, EXIT_FAILED, EXIT_OK};
use anvil_front::ir::Program;
use anvil_interp::{Property, TestFail};
use serde_json::json;
use std::path::Path;

/// Cases per property test unless `cases` says otherwise.
pub const PROPERTY_CASES: u64 = 100;

macro_rules! try_report {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

/// One test's interpreter result, as plain data (values are shown as Anvil source).
#[derive(Clone)]
pub(crate) struct Ran {
    pub name: String,
    pub property: bool,
    /// None if it passed; the panic or step-limit message if not.
    pub failure: Option<String>,
    pub stopped: bool,
    pub output: String,
    /// For a failed property: the shrunk inputs, and which case first failed.
    pub inputs: Option<String>,
    pub case: Option<u64>,
    pub shrinks: u64,
}

/// Runs every test of `program` in the interpreter: plain tests once, property tests on `cases`
/// inputs seeded by their name and `seed`. `rec`, when given, counts what ran.
pub(crate) fn run_all(
    program: &Program,
    cases: u64,
    seed: u64,
    max_steps: Option<u64>,
    mut rec: Option<&mut anvil_interp::Recorder>,
    ffi: &mut Option<crate::Ffi>,
    root: &Path,
) -> Vec<Ran> {
    let mut out = Vec::new();
    for (i, t) in program.tests.iter().enumerate() {
        let mut ran = Ran {
            name: t.name.clone(),
            property: !t.inputs.is_empty(),
            failure: None,
            stopped: false,
            output: String::new(),
            inputs: None,
            case: None,
            shrinks: 0,
        };
        if t.inputs.is_empty() {
            let mut buf = Vec::new();
            let host = crate::checked_host(ffi, root, &[], None);
            match anvil_interp::run_test_with(program, i, &[], &mut buf, max_steps, rec.as_deref_mut(), host) {
                Ok(()) => {}
                Err(TestFail::Panic(m)) => ran.failure = Some(m),
                Err(TestFail::Stopped(m)) => (ran.failure, ran.stopped) = (Some(m), true),
            }
            ran.output = String::from_utf8_lossy(&buf).into_owned();
        } else {
            let host = crate::checked_host(ffi, root, &[], None);
            match anvil_interp::run_property(program, i, cases, seed, max_steps, rec.as_deref_mut(), host) {
                Property::Passed { .. } => {}
                Property::Stopped(m) => (ran.failure, ran.stopped) = (Some(m), true),
                Property::Failed { case, inputs, message, output, shrinks } => {
                    let shown: Vec<String> = t
                        .inputs
                        .iter()
                        .zip(&inputs)
                        .map(|(l, v)| {
                            format!("{}: {}", t.locals[*l].name, anvil_interp::show(v, &t.locals[*l].ty, program))
                        })
                        .collect();
                    ran.failure = Some(message);
                    ran.output = output;
                    ran.inputs = Some(shown.join(", "));
                    ran.case = Some(case);
                    ran.shrinks = shrinks;
                }
            }
        }
        out.push(ran);
    }
    out
}

/// What the native executable did for one test, run in `root` without input.
fn native_result(exe: &Path, index: usize, root: &Path) -> Result<golden::Output, String> {
    golden::run_native(exe, &[&index.to_string()], root, &[])
}

pub fn test(
    root: &Path,
    file: &str,
    max_steps: Option<u64>,
    native: bool,
    cases: Option<u64>,
    seed: Option<u64>,
) -> Report {
    let (_, program) = try_report!(compile(root, file));
    let cases = cases.unwrap_or(PROPERTY_CASES);
    if cases == 0 {
        return Report::usage("--cases must be at least 1");
    }
    let seed = seed.unwrap_or(0);
    let p2 = program.clone();
    let mut ffi = match crate::c_functions(&program) {
        Ok(f) => f,
        Err(e) => return Report::failed(json!({ "ok": false, "error": e }), format!("error: {e}\n"), EXIT_FAILED),
    };
    let root2 = root.to_path_buf();
    let ran = anvil_interp::run_on_big_stack(move || run_all(&p2, cases, seed, max_steps, None, &mut ffi, &root2));

    // Natively: the plain tests, in one executable, compared with the interpreter.
    let mut native_notes: Vec<Option<String>> = vec![None; ran.len()];
    let mut disagree: Vec<Option<String>> = vec![None; ran.len()];
    if native && ran.iter().any(|r| !r.property) {
        let scratch = std::env::temp_dir().join(format!("anvil_tests_{}_{}", std::process::id(), unique()));
        let exe = scratch.join(format!("tests{}", std::env::consts::EXE_SUFFIX));
        let built = std::fs::create_dir_all(&scratch)
            .map_err(|e| e.to_string())
            .and_then(|_| anvil_codegen::build_test_exe(&program, &exe, &scratch.join("work")));
        match built {
            Err(e) => {
                let _ = std::fs::remove_dir_all(&scratch);
                return Report::failed(json!({ "ok": false, "error": e }), format!("error: {e}\n"), EXIT_FAILED);
            }
            Ok(indices) => {
                for i in indices {
                    let r = &ran[i];
                    if r.stopped {
                        native_notes[i] = Some("skipped: the interpreter reached the step limit".into());
                        continue;
                    }
                    match native_result(&exe, i, root) {
                        Err(e) => {
                            native_notes[i] = Some(format!("error: {e}"));
                            disagree[i] = Some(format!("native {e}"));
                        }
                        Ok(o) => {
                            native_notes[i] = Some(match &o.panic {
                                None => "ok".into(),
                                Some(m) => format!("panic: {m}"),
                            });
                            let same = o.panic == r.failure && o.stdout == r.output;
                            if !same {
                                let theirs =
                                    o.panic.as_deref().map_or("passes".into(), |m| format!("panics with `{m}`"));
                                let ours =
                                    r.failure.as_deref().map_or("passes".into(), |m| format!("panics with `{m}`"));
                                let what = if o.panic == r.failure {
                                    "prints differently".to_string()
                                } else {
                                    format!("{theirs}; the interpreter {ours}")
                                };
                                disagree[i] = Some(format!("the backends disagree: natively it {what}"));
                            }
                        }
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    let mut tests = Vec::new();
    let mut text = String::new();
    let mut failed = 0;
    for (i, r) in ran.iter().enumerate() {
        let problem = disagree[i].clone().or_else(|| r.failure.clone());
        let mut v = json!({ "name": r.name, "ok": problem.is_none(), "property": r.property });
        if r.property {
            v["cases"] = json!(cases);
        }
        if let Some(n) = &native_notes[i] {
            v["native"] = json!(n);
        }
        if let Some(m) = &problem {
            failed += 1;
            v["panic"] = json!(m);
            v["output"] = json!(r.output);
            text.push_str(&format!("FAIL \"{}\": {m}\n", r.name));
            if let (Some(inputs), Some(case)) = (&r.inputs, r.case) {
                v["inputs"] = json!(inputs);
                v["case"] = json!(case);
                v["shrinks"] = json!(r.shrinks);
                text.push_str(&format!(
                    "  for all: {inputs}\n  (case {} of {cases}, shrunk {} time(s); seed {seed})\n",
                    case + 1,
                    r.shrinks
                ));
            }
            for line in r.output.lines() {
                text.push_str(&format!("  | {line}\n"));
            }
        }
        tests.push(v);
    }
    let passed = ran.len() - failed;
    let properties = ran.iter().filter(|r| r.property).count();
    let mut summary = format!("{passed} passed, {failed} failed");
    if properties > 0 {
        summary.push_str(&format!("; {properties} property test(s), {cases} cases each"));
    }
    if native && ran.iter().any(|r| !r.property) {
        summary.push_str("; plain tests also native");
    }
    text.push_str(&summary);
    text.push('\n');
    let json = json!({
        "ok": failed == 0, "passed": passed, "failed": failed, "native": native, "cases": cases, "seed": seed,
        "tests": tests,
    });
    Report {
        json,
        stdout: text,
        stderr: String::new(),
        exit: if failed == 0 { EXIT_OK } else { EXIT_FAILED },
        failed: failed > 0,
    }
}
