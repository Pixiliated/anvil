//! `anvil measure`: programs built and run natively, as their goldens run them, with what their
//! objects asked of the heap as the runtime counts it (bytes asked for, the most held at once,
//! objects made), beside the interpreter's steps and modelled bytes. Heap numbers are the same on
//! every run, since one worker runs at a time; the time is reported, and never compared. With
//! `native_only` the interpreter does not run: nothing to wait for on a big input, and nothing
//! compared.

use crate::golden::{self, Input};
use crate::project::Project;
use crate::{Files, ProgramInput, Report, EXIT_FAILED, EXIT_OK};
use anvil_front::ir::Program;
use serde_json::{json, Value};
use std::path::Path;

/// What the runtime counted, as it wrote it at exit.
struct Heap {
    allocated: u64,
    peak: u64,
    objects: u64,
}

fn parse_heap(text: &str) -> Option<Heap> {
    let mut n = text.split_whitespace().map(|w| w.parse::<u64>());
    let heap = Heap { allocated: n.next()?.ok()?, peak: n.next()?.ok()?, objects: n.next()?.ok()? };
    n.next().is_none().then_some(heap)
}

/// Builds `program` and runs it as a golden runs, returning its output, heap and milliseconds.
fn run(program: &Program, project: &Project, file: &str, input: &Input) -> Result<(golden::Output, Heap, u64), String> {
    let scratch = std::env::temp_dir().join(format!("anvil_measure_{}_{}", std::process::id(), crate::unique()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let result = (|| {
        let exe = scratch.join(format!("program{}", std::env::consts::EXE_SUFFIX));
        anvil_codegen::build_exe(program, &exe, &scratch.join("work"), false)?;
        let counts = scratch.join("heap.txt");
        let mut env = input.env.clone();
        env.push(("ANVIL_MEASURE".to_string(), counts.display().to_string()));
        env.push(("ANVIL_THREADS".to_string(), "1".to_string()));
        let args: Vec<&str> = input.args.iter().map(String::as_str).collect();
        let started = std::time::Instant::now();
        let out = golden::run_native(&exe, &args, &project.root, &input.stdin, &env)?;
        let ms = started.elapsed().as_millis() as u64;
        let text = std::fs::read_to_string(&counts).map_err(|e| format!("{file}: no heap counts: {e}"))?;
        let heap = parse_heap(&text).ok_or_else(|| format!("{file}: unreadable heap counts: {text}"))?;
        Ok((out, heap, ms))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// One program's numbers, or why it could not be measured.
fn measure_file(
    project: &Project,
    file: &str,
    max_steps: Option<u64>,
    input: &ProgramInput,
    native_only: bool,
) -> Result<Option<Value>, String> {
    let src = Files::disk(&project.root).load(file).map_err(|r| r.json["error"].as_str().unwrap_or("").to_string())?;
    let program = match anvil_front::compile_with(&src, &crate::loader(&project.root)) {
        (_, Ok(p)) => p,
        (_, Err(d)) => {
            return Err(format!(
                "{file} does not compile: {} error(s), the first {}: {}",
                d.len(),
                d[0].code,
                d[0].message
            ))
        }
    };
    if program.main.is_none() {
        return Ok(None);
    }
    let input = if input.given() {
        let stdin = input.file_bytes(&project.root).map_err(|r| r.json["error"].as_str().unwrap_or("").to_string())?;
        Input { args: input.args.clone(), stdin: stdin.unwrap_or_default(), env: Vec::new() }
    } else {
        golden::read_input(project, file)?
    };
    if native_only {
        let (_, heap, ms) = run(&program, project, file, &input)?;
        return Ok(Some(json!({
            "file": file, "steps": null, "bytes": null, "copied": null,
            "native": { "allocated": heap.allocated, "peak": heap.peak, "objects": heap.objects, "ms": ms, "differs": null },
        })));
    }
    let (want, counts) = golden::interpret(program.clone(), max_steps, &project.root, &input)?;
    let (got, heap, ms) = run(&program, project, file, &input)?;
    let same = golden::difference(&want, &got);
    Ok(Some(json!({
        "file": file, "steps": counts.steps, "bytes": counts.bytes, "copied": counts.copied,
        "native": {
            "allocated": heap.allocated, "peak": heap.peak, "objects": heap.objects, "ms": ms,
            "differs": same,
        },
    })))
}

/// Every project file with a main (or one file), run natively and measured.
pub fn measure(
    root: &Path,
    file: Option<&str>,
    max_steps: Option<u64>,
    input: &ProgramInput,
    native_only: bool,
) -> Report {
    if file.is_none() && input.given() {
        return Report::usage("arguments and --input are for one file; name it");
    }
    let project = match Project::discover(root) {
        Ok(p) => p,
        Err(e) => return Report::usage(e),
    };
    let files = match crate::proj::select(&project, root, file) {
        Ok(f) => f,
        Err(e) => return Report::usage(e),
    };
    let mut rows = Vec::new();
    for f in &files {
        match measure_file(&project, f, max_steps, input, native_only) {
            Ok(Some(row)) => rows.push(row),
            Ok(None) if file.is_some() => return Report::usage(format!("{f} has no main to measure")),
            Ok(None) => {}
            Err(e) => return Report::error(e),
        }
    }
    let mut text = format!(
        "{:>10} {:>12} {:>10} {:>12} {:>12} {:>9} {:>7}  file\n",
        "steps", "model bytes", "copied", "heap bytes", "peak bytes", "objects", "ms"
    );
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let counted = |v: &Value| v.as_u64().map_or("-".to_string(), |x| x.to_string());
    let mut bad = 0;
    for r in &rows {
        let h = &r["native"];
        text.push_str(&format!(
            "{:>10} {:>12} {:>10} {:>12} {:>12} {:>9} {:>7}  {}\n",
            counted(&r["steps"]),
            counted(&r["bytes"]),
            counted(&r["copied"]),
            n(&h["allocated"]),
            n(&h["peak"]),
            n(&h["objects"]),
            n(&h["ms"]),
            r["file"].as_str().unwrap_or("")
        ));
        if let Some(d) = h["differs"].as_str() {
            bad += 1;
            text.push_str(&format!("  native differs from the interpreter: {d}\n"));
        }
    }
    text.push_str(&format!("{} program(s) measured; heap numbers repeat exactly, time does not\n", rows.len()));
    let json = json!({ "ok": bad == 0, "programs": rows });
    let exit = if bad == 0 { EXIT_OK } else { EXIT_FAILED };
    Report { json, stdout: text, stderr: String::new(), exit, failed: bad > 0 }
}
