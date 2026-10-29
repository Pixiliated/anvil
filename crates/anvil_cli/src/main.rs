//! The `anvil` command. Quiet by default (failures and one summary line), `--json` for tools.
//! Exit codes: 0 success, 1 compile errors or failing tests, 2 usage errors; `run` passes through
//! the program's own exit code, 101 on panic.

use anvil_front::diag::{self, Diagnostic};
use anvil_front::ir::{Program, PANIC_EXIT_CODE};
use anvil_front::span::SourceFile;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SPEC: &str = include_str!("../../../docs/SPEC.md");

const USAGE: &str = "\
usage: anvil <command> [args]

  check <file.anv> [--json]               type-check only
  run   <file.anv> [--json]               interpret main (reference semantics)
  build <file.anv> [-o out.exe] [--asm] [--json]
                                          compile main to a native executable;
                                          --asm prints each function's machine code
  test  <file.anv> [--json]               run every test block
  explain [CODE] [--json]                 explain an error code (no code: list all)
  spec                                    print the language specification";

struct Args {
    command: String,
    file: Option<String>,
    out: Option<String>,
    json: bool,
    asm: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or("missing command")?;
    let mut a = Args { command, file: None, out: None, json: false, asm: false };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => a.json = true,
            "--asm" => a.asm = true,
            "-o" => a.out = Some(it.next().ok_or("-o needs a path")?),
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
            s => {
                if a.file.is_some() {
                    return Err(format!("unexpected argument `{s}`"));
                }
                a.file = Some(s.to_string());
            }
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if (args.out.is_some() || args.asm) && args.command != "build" {
        eprintln!("error: -o and --asm only apply to build");
        return ExitCode::from(2);
    }
    match args.command.as_str() {
        "spec" => {
            print!("{SPEC}");
            ExitCode::SUCCESS
        }
        "explain" => explain(&args),
        "check" | "run" | "build" | "test" => {
            let Some(file) = &args.file else {
                eprintln!("error: {} needs a source file\n\n{USAGE}", args.command);
                return ExitCode::from(2);
            };
            let text = match std::fs::read_to_string(file) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: cannot read {file}: {e}");
                    return ExitCode::from(2);
                }
            };
            let src = SourceFile::new(file.replace('\\', "/"), text);
            let program = match anvil_front::compile(&src) {
                Ok(p) => p,
                Err(diags) => return report(&src, &diags, args.json),
            };
            match args.command.as_str() {
                "check" => {
                    if args.json {
                        println!("{}", json!({ "ok": true, "diagnostics": [] }));
                    }
                    ExitCode::SUCCESS
                }
                "run" => run(&src, program, args.json),
                "build" => build(&src, &program, file, &args),
                _ => test(program, args.json),
            }
        }
        other => {
            eprintln!("error: unknown command `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn report(src: &SourceFile, diags: &[Diagnostic], json: bool) -> ExitCode {
    if json {
        let ds: Vec<_> = diags.iter().map(|d| diag::to_json(src, d)).collect();
        println!("{}", json!({ "ok": false, "diagnostics": ds }));
    } else {
        for d in diags {
            eprintln!("{}", diag::render(src, d));
        }
        eprintln!("{} error(s)", diags.len());
    }
    ExitCode::from(1)
}

fn no_main(src: &SourceFile, json: bool) -> ExitCode {
    let d = Diagnostic::new("E0350", anvil_front::span::Span::new(0, 0), "this file has no main function");
    report(src, &[d], json)
}

fn run(src: &SourceFile, program: Program, json: bool) -> ExitCode {
    if program.main.is_none() {
        return no_main(src, json);
    }
    let outcome = anvil_interp::run_on_big_stack(move || {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        match anvil_interp::run_main(&program, &mut lock) {
            anvil_interp::Outcome::Exit(c) => Ok(c),
            anvil_interp::Outcome::Panicked(m) => Err(m),
        }
    });
    match outcome {
        Ok(code) => ExitCode::from(code as u8),
        Err(m) => {
            if json {
                eprintln!("{}", json!({ "panic": m }));
            } else {
                eprintln!("panic: {m}");
            }
            ExitCode::from(PANIC_EXIT_CODE as u8)
        }
    }
}

fn build(src: &SourceFile, program: &Program, file: &str, args: &Args) -> ExitCode {
    let json = args.json;
    if program.main.is_none() {
        return no_main(src, json);
    }
    let out: PathBuf = match &args.out {
        Some(o) => PathBuf::from(o),
        None => Path::new(file).with_extension("exe"),
    };
    let work = std::env::temp_dir().join(format!("anvil_build_{}", std::process::id()));
    let result = anvil_codegen::build_exe(program, &out, &work, args.asm);
    let _ = std::fs::remove_dir_all(&work);
    match result {
        Ok(listing) => {
            if json {
                let mut v = json!({ "ok": true, "output": out.display().to_string() });
                if args.asm {
                    v["asm"] = json!(listing);
                }
                println!("{v}");
            } else if args.asm {
                print!("{listing}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                println!("{}", json!({ "ok": false, "error": e }));
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::from(1)
        }
    }
}

fn test(program: Program, json: bool) -> ExitCode {
    let results = anvil_interp::run_on_big_stack(move || {
        let mut results = Vec::new();
        for (i, t) in program.tests.iter().enumerate() {
            let mut out = Vec::new();
            let r = anvil_interp::run_test(&program, i, &mut out);
            results.push((t.name.clone(), r, String::from_utf8_lossy(&out).into_owned()));
        }
        results
    });
    let failed = results.iter().filter(|r| r.1.is_err()).count();
    let passed = results.len() - failed;
    if json {
        let tests: Vec<_> = results
            .iter()
            .map(|(name, r, out)| match r {
                Ok(()) => json!({ "name": name, "ok": true }),
                Err(m) => json!({ "name": name, "ok": false, "panic": m, "output": out }),
            })
            .collect();
        println!("{}", json!({ "ok": failed == 0, "passed": passed, "failed": failed, "tests": tests }));
    } else {
        for (name, r, out) in &results {
            if let Err(m) = r {
                println!("FAIL \"{name}\": {m}");
                for line in out.lines() {
                    println!("  | {line}");
                }
            }
        }
        println!("{passed} passed, {failed} failed");
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn explain(args: &Args) -> ExitCode {
    match &args.file {
        Some(code) => match diag::explain(code) {
            Some(text) => {
                if args.json {
                    println!("{}", json!({ "code": code, "explanation": text }));
                } else {
                    println!("{code}: {text}");
                }
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("error: unknown error code `{code}`");
                ExitCode::from(2)
            }
        },
        None => {
            if args.json {
                let all: Vec<_> =
                    diag::ALL_CODES.iter().map(|c| json!({ "code": c, "explanation": diag::explain(c) })).collect();
                println!("{}", serde_json::Value::Array(all));
            } else {
                for c in diag::ALL_CODES {
                    println!("{c}: {}", diag::explain(c).unwrap());
                }
            }
            ExitCode::SUCCESS
        }
    }
}
