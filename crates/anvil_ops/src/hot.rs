//! `anvil hot`: where a compiled program's time goes. Builds the program as `build` does, runs it
//! with the runtime sampling where it is about every millisecond (ANVIL_SAMPLE), and names what
//! the samples landed in with the linker's symbols: the program's functions, and what the runtime does
//! for them. `profile` counts steps in the interpreter, exactly; this measures the real thing,
//! so its numbers differ from run to run and are never compared. Small functions are compiled into
//! their callers, so they show there. It also says which arrays the compiler could not prove have
//! one owner in a loop, since that keeps each round of the loop checking.

use crate::golden::{self, Input};
use crate::project::Project;
use crate::{ProgramInput, Report, EXIT_FAILED};
use anvil_front::ir::Program;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// What a runtime or C library function is doing, for someone who did not write it, and what can
/// be done about it.
const SYSTEM: (&str, &str) = ("the system: files, memory from the OS, waiting", "");
const LIBRARY: (&str, &str) = ("the C library", "");
const BYTES: (&str, &str) = ("copying and comparing bytes", "text and arrays being copied or compared; `anvil profile FILE` names where values are copied");
const COPYING: (&str, &str) = ("copying arrays and maps", "a write to something shared copies it first; `anvil profile FILE` names each copy and why");
const OBJECTS: (&str, &str) = ("making and freeing objects", "short-lived arrays and text; numbers in a flat array make none");
const MAPS: (&str, &str) = ("maps", "");
const SORTING: (&str, &str) = ("sorting", "");
const TEXT: (&str, &str) = ("text", "");
const PRINTING: (&str, &str) = ("printing", "");
const FILES: (&str, &str) = ("reading and writing files", "");

/// The group of a symbol that is not one of the program's functions.
fn group_of(name: &str) -> (&'static str, &'static str) {
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if matches!(name, "anvil_arr_unique" | "retain_elements" | "map_clone" | "deep_copy" | "slice_copy" | "anvil_arr_slice") {
        COPYING
    } else if starts(&["anvil_map_", "hash_", "table_", "key_before", "sort_positions"]) {
        MAPS
    } else if starts(&["pool_", "str_new", "str_from", "arr_init", "anvil_retain", "anvil_release", "anvil_arr_new", "anvil_fn_new", "anvil_arr_push_slot", "xmalloc", "malloc", "free", "realloc", "calloc", "_malloc", "_free", "_realloc", "_calloc", "_heap"]) {
        OBJECTS
    } else if starts(&["anvil_arr_sorted", "merge_sort"]) {
        SORTING
    } else if starts(&["anvil_print", "anvil_eprint", "fwrite", "_fwrite", "fputc", "_fputc", "_write", "fflush", "_fflush", "__stdio"]) {
        PRINTING
    } else if starts(&["anvil_read", "anvil_write", "read_any", "fread", "_fread", "_read"]) {
        FILES
    } else if starts(&["anvil_str", "anvil_bytes", "anvil_f64", "anvil_arr_join", "format_f64", "i64_digits", "find_at", "utf8_valid", "strtod", "snprintf"]) {
        TEXT
    } else if starts(&["mem", "__mem", "Mov", "Ymm", "Xmm", "mcmp", "__avx", "__sse", "_mem", "head_loop", "main_loop", "found"]) {
        BYTES
    } else {
        LIBRARY
    }
}

/// The source name of a generated symbol (`anv12_name`), by its function's index.
fn program_function(program: &Program, symbol: &str) -> Option<String> {
    // The program's entry, where a C compiler may have put all of it.
    if symbol == "anvil_program" {
        return Some("main".to_string());
    }
    // The release backend's C puts its own number first: `F95_anv21_process`.
    let symbol = symbol.strip_prefix('F').and_then(|s| s.split_once('_')).map_or(symbol, |(_, name)| name);
    let rest = symbol.strip_prefix("anv")?;
    let digits = rest.find('_')?;
    let i: usize = rest[..digits].parse().ok()?;
    program.funcs.get(i).map(|f| f.name.clone())
}

/// Runs the program as tests do (writes stay in memory), sampling; returns the exit code.
fn run(exe: &Path, input: &Input, cwd: &Path, samples: &Path) -> Result<i32, String> {
    let mut child = Command::new(std::path::absolute(exe).map_err(|e| e.to_string())?)
        .args(&input.args)
        .current_dir(cwd)
        .env_clear()
        .envs(input.env.iter().map(|(k, v)| (k, v)))
        .env("ANVIL_FS", "memory")
        .env("ANVIL_SAMPLE", samples)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
    let mut pipe = child.stdin.take().expect("stdin is piped");
    let bytes = input.stdin.clone();
    let writer = std::thread::spawn(move || {
        // A program may exit without reading it all; that is not an error.
        let _ = pipe.write_all(&bytes);
    });
    let status = child.wait().map_err(|e| format!("cannot wait for {}: {e}", exe.display()))?;
    let _ = writer.join();
    status.code().ok_or_else(|| format!("{} ended without an exit code", exe.display()))
}

/// Sample addresses from the runtime's file: the address `anvil_program` had, then one per line.
fn parse_samples(text: &str) -> Option<(u64, Vec<u64>)> {
    let mut lines = text.lines().map(|l| u64::from_str_radix(l, 16));
    let base = lines.next()?.ok()?;
    let samples = lines.collect::<Result<Vec<u64>, _>>().ok()?;
    Some((base, samples))
}

/// One row per function of the program and per kind of runtime work, largest first.
fn rows(program: &Program, symbols: &[(u64, String)], base: u64, samples: &[u64]) -> Result<Vec<Value>, String> {
    let linked = symbols
        .iter()
        .find(|(_, n)| n == "anvil_program" || n == "_anvil_program")
        .ok_or("the linker's symbols do not name anvil_program")?
        .0;
    let last = symbols.last().map_or(0, |(a, _)| *a);
    // (what it is, hint), to (samples, samples per symbol).
    let mut counts: BTreeMap<(String, String, &'static str), (u64, BTreeMap<String, u64>)> = BTreeMap::new();
    for s in samples {
        let at = (*s).wrapping_sub(base).wrapping_add(linked);
        let i = symbols.partition_point(|(a, _)| *a <= at);
        let found = (i > 0 && at <= last + (1 << 20)).then(|| symbols[i - 1].1.as_str());
        let (what, hint, kind, name) = match found {
            None => (SYSTEM.0.to_string(), SYSTEM.1.to_string(), "system", String::new()),
            Some(symbol) => match program_function(program, symbol) {
                Some(f) => (f, String::new(), "function", String::new()),
                None => {
                    let (g, h) = group_of(symbol);
                    (g.to_string(), h.to_string(), "runtime", symbol.to_string())
                }
            },
        };
        let e = counts.entry((what, hint, kind)).or_default();
        e.0 += 1;
        if !name.is_empty() {
            *e.1.entry(name).or_default() += 1;
        }
    }
    let total = samples.len().max(1) as f64;
    let percent = |n: u64| (n as f64 * 1000.0 / total).round() / 10.0;
    let mut out: Vec<(u64, Value)> = counts
        .into_iter()
        .map(|((what, hint, kind), (n, names))| {
            let mut names: Vec<(String, u64)> = names.into_iter().collect();
            names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let names: Vec<Value> = names.iter().take(4).map(|(s, c)| json!({ "symbol": s, "percent": percent(*c) })).collect();
            (n, json!({ "what": what, "kind": kind, "percent": percent(n), "symbols": names, "hint": hint }))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1["what"].as_str().cmp(&b.1["what"].as_str())));
    Ok(out.into_iter().map(|(_, v)| v).collect())
}

/// Where a compiled program's time goes.
pub fn hot(root: &Path, file: &str, input: &ProgramInput, release: bool) -> Report {
    let program = match crate::with_main(root, file) {
        Ok((_, program)) => program,
        Err(r) => return r,
    };
    let run_input = if input.given() {
        let stdin = match input.file_bytes(root) {
            Ok(b) => b,
            Err(r) => return r,
        };
        Input { args: input.args.clone(), stdin: stdin.unwrap_or_default(), env: Vec::new() }
    } else {
        match Project::discover(root).map(|p| golden::read_input(&p, file)) {
            Ok(Ok(i)) => i,
            Ok(Err(e)) => return Report::error(e),
            Err(_) => Input::default(),
        }
    };
    let scratch = std::env::temp_dir().join(format!("anvil_hot_{}_{}", std::process::id(), crate::unique()));
    let result = (|| {
        std::fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
        let exe = scratch.join(format!("program{}", std::env::consts::EXE_SUFFIX));
        let built = anvil_codegen::build_exe_with_symbols(&program, &exe, &scratch.join("work"), release)?;
        let sample_file = scratch.join("samples.txt");
        let started = std::time::Instant::now();
        let code = run(&exe, &run_input, root, &sample_file)?;
        let ms = started.elapsed().as_millis() as u64;
        let text = std::fs::read_to_string(&sample_file).map_err(|e| format!("{file}: no samples: {e}"))?;
        let (base, samples) = parse_samples(&text).ok_or_else(|| format!("{file}: unreadable samples"))?;
        let table = rows(&program, &built.symbols, base, &samples)?;
        let notes: Vec<Value> =
            built.notes.iter().map(|n| json!({ "function": n.function, "at": n.at, "local": n.local })).collect();
        Ok::<_, String>(json!({ "ok": true, "exit": code, "ms": ms, "samples": samples.len(), "rows": table, "unproved": notes }))
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let v = match result {
        Ok(v) => v,
        Err(e) => return Report::failed(json!({ "ok": false, "error": e }), format!("error: {e}\n"), EXIT_FAILED),
    };
    let mut text = format!(
        "{}: {} samples over {} ms, exit code {}; time differs from run to run\n",
        file, v["samples"], v["ms"], v["exit"]
    );
    if v["samples"] == 0 {
        text.push_str("it ended too soon to sample: give it a bigger input\n");
    }
    for r in v["rows"].as_array().unwrap() {
        let names: Vec<String> = r["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| format!("{} {}%", s["symbol"].as_str().unwrap_or(""), s["percent"]))
            .collect();
        let detail = if names.is_empty() { String::new() } else { format!("  ({})", names.join(", ")) };
        text.push_str(&format!("{:>5.1}%  {}{}\n", r["percent"].as_f64().unwrap_or(0.0), r["what"].as_str().unwrap_or(""), detail));
        if let Some(h) = r["hint"].as_str().filter(|h| !h.is_empty()) {
            text.push_str(&format!("        {h}\n"));
        }
    }
    let unproved = v["unproved"].as_array().unwrap();
    if !unproved.is_empty() {
        text.push_str("\nArrays checked for a second owner on every round of a loop:\n");
        for n in unproved {
            text.push_str(&format!(
                "  {}  `{}` in {}\n",
                n["at"].as_str().unwrap_or(""),
                n["local"].as_str().unwrap_or(""),
                n["function"].as_str().unwrap_or("")
            ));
        }
        text.push_str("  An array made in the function, or returned new by a call, has one owner from the start;\n  a parameter or a copy of one does not until its first write, so copy it before the loop.\n");
    }
    Report::ok(v, text)
}
