//! Watching a program run in the interpreter: `trace` (a timeline of calls, returns and prints)
//! and `profile` (calls, steps and bytes allocated per function). Both count steps, not time, so a program gives
//! the same answer on every machine. They run main, or one test named with `test`. A panic or the
//! step limit ends the run but is not a failure of the command: it is part of what was observed.

use crate::golden::Input;
use crate::{compile, ProgramInput, Report};
use anvil_front::check::closest;
use anvil_front::ir::Program;
use anvil_interp::{show, Event, Outcome, Recorder};
use serde_json::{json, Value};
use std::path::Path;

/// Events a trace keeps when no limit is given.
pub const TRACE_LIMIT: u64 = 1000;

macro_rules! try_report {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

/// How the observed run ended.
enum Ended {
    Exit(i32),
    Passed,
    Panic(String),
    Stopped(String),
}

impl Ended {
    fn to_json(&self) -> Value {
        match self {
            Ended::Exit(code) => json!({ "exit": code }),
            Ended::Passed => json!({ "passed": true }),
            Ended::Panic(m) => json!({ "panic": m }),
            Ended::Stopped(m) => json!({ "stopped": m }),
        }
    }

    fn text(&self) -> String {
        match self {
            Ended::Exit(code) => format!("exit {code}"),
            Ended::Passed => "the test passed".into(),
            Ended::Panic(m) => format!("panic: {m}"),
            Ended::Stopped(m) => m.clone(),
        }
    }
}

/// Main (None) or the index of the test named `test`.
fn target(program: &Program, file: &str, test: Option<&str>) -> Result<Option<usize>, Report> {
    match test {
        None if program.main.is_none() => {
            Err(Report::usage(format!("{file} has no main; name one of its tests with `test`")))
        }
        None => Ok(None),
        Some(name) => match program.tests.iter().position(|t| t.name == name) {
            Some(i) => Ok(Some(i)),
            None => {
                let names: Vec<String> = program.tests.iter().map(|t| format!("\"{}\"", t.name)).collect();
                let list = if names.is_empty() { "it has none".to_string() } else { names.join(", ") };
                Err(Report::usage(format!("{file} has no test \"{name}\"; its tests: {list}")))
            }
        },
    }
}

/// main's input for a trace or profile; tests run without one.
fn given(root: &Path, which: Option<usize>, input: &ProgramInput) -> Result<Input, Report> {
    if which.is_some() && input.given() {
        return Err(Report::usage("arguments and --input are for main; tests run without them"));
    }
    let stdin = input.file_bytes(root)?.unwrap_or_default();
    Ok(Input { args: input.args.clone(), stdin, env: Vec::new() })
}

fn run_name(program: &Program, which: Option<usize>) -> String {
    match which {
        None => "main".into(),
        Some(i) => format!("test \"{}\"", program.tests[i].name),
    }
}

/// Runs main or a test with the recorder `make` builds, then hands the program, the recorder and
/// how the run ended to `read`, all on the interpreter's big-stack thread (values are not Send).
#[allow(clippy::too_many_arguments)]
fn observe<T: Send + 'static>(
    program: Program,
    which: Option<usize>,
    max_steps: Option<u64>,
    root: &Path,
    input: Input,
    make: impl FnOnce(&Program) -> Recorder + Send + 'static,
    read: impl FnOnce(&Program, &Recorder, Ended) -> T + Send + 'static,
) -> Result<T, String> {
    let mut ffi = crate::c_functions(&program)?;
    let root = root.to_path_buf();
    Ok(anvil_interp::run_on_big_stack(move || {
        let mut rec = make(&program);
        let mut sink = Vec::new();
        let mut err_sink = Vec::new();
        let mut stdin = std::io::Cursor::new(input.stdin.as_slice());
        let host = crate::checked_host(&mut ffi, &root, &input.args, Some(&mut stdin), &mut err_sink, &input.env);
        let ended = match which {
            None => match anvil_interp::run_main_recorded(&program, &mut sink, max_steps, Some(&mut rec), host) {
                Outcome::Exit(code) => Ended::Exit(code),
                Outcome::Panicked(m) => Ended::Panic(m),
                Outcome::OutOfSteps(n) => Ended::Stopped(anvil_interp::step_limit_message(n)),
            },
            Some(i) => match anvil_interp::run_test_recorded(&program, i, &mut sink, max_steps, Some(&mut rec), host) {
                Ok(()) => Ended::Passed,
                // run_test reports the step limit as its message; tell it apart from a panic.
                Err(m) if max_steps.is_some_and(|n| m == anvil_interp::step_limit_message(n)) => Ended::Stopped(m),
                Err(m) => Ended::Panic(m),
            },
        };
        read(&program, &rec, ended)
    }))
}

pub fn trace(
    root: &Path,
    file: &str,
    test: Option<&str>,
    only: Option<&str>,
    limit: Option<u64>,
    max_steps: Option<u64>,
    input: &ProgramInput,
) -> Report {
    let (_, program) = try_report!(compile(root, file));
    let which = try_report!(target(&program, file, test));
    let given = try_report!(given(root, which, input));
    let limit = limit.unwrap_or(TRACE_LIMIT);
    if limit == 0 {
        return Report::usage("the trace limit must be at least 1");
    }
    let only = match only {
        None => None,
        Some(list) => {
            let mut keep = vec![false; program.funcs.len()];
            let names: Vec<String> = program.funcs.iter().map(|f| f.name.clone()).collect();
            for name in list.split(',').map(str::trim) {
                match names.iter().position(|n| n == name) {
                    Some(i) => keep[i] = true,
                    None => {
                        let hint = match closest(name, names.iter()) {
                            Some(s) => format!("; did you mean `{s}`?"),
                            None => String::new(),
                        };
                        return Report::usage(format!("{file} has no function `{name}`{hint}"));
                    }
                }
            }
            Some(keep)
        }
    };
    let run = run_name(&program, which);
    let (events, text, steps, dropped, ended) = try_report!(observe(
        program,
        which,
        max_steps,
        root,
        given,
        move |p| Recorder::tracing(p, only, limit as usize),
        |p, rec, ended| {
            let t = rec.trace.as_ref().unwrap();
            let mut events = Vec::new();
            let mut text = String::new();
            for e in &t.events {
                let (v, indent, line) = event(p, e);
                let step = v["step"].as_u64().unwrap();
                text.push_str(&format!("{step:>8}  {}{line}\n", "  ".repeat(indent)));
                events.push(v);
            }
            (events, text, rec.steps, t.dropped, ended)
        },
    )
    .map_err(Report::error));
    let mut text = format!("    step  trace of {run} in {file}\n{text}");
    if dropped > 0 {
        text.push_str(&format!("          ... {dropped} more event(s) not kept (the limit is {limit})\n"));
    }
    text.push_str(&format!("{steps:>8}  end: {}\n", ended.text()));
    let json = json!({
        "ok": true, "file": file, "run": run, "steps": steps, "events": events, "limit": limit,
        "dropped": dropped, "end": ended.to_json(),
    });
    Report::ok(json, text)
}

/// One event as JSON, with its indent and line for the text timeline.
fn event(p: &Program, e: &Event) -> (Value, usize, String) {
    match e {
        Event::Call { step, depth, func, args } => {
            let f = &p.funcs[*func];
            let args: Vec<String> = f
                .params
                .iter()
                .zip(args)
                .map(|(l, v)| format!("{}: {}", f.locals[*l].name, show(v, &f.locals[*l].ty, p)))
                .collect();
            let args = args.join(", ");
            let line = format!("{}({args})", f.name);
            (json!({ "step": step, "depth": depth, "call": f.name, "args": args }), depth - 1, line)
        }
        Event::Return { step, depth, func, value } => {
            let f = &p.funcs[*func];
            let value = show(value, &f.ret, p);
            let line = format!("{} returns {value}", f.name);
            (json!({ "step": step, "depth": depth, "return": f.name, "value": value }), depth - 1, line)
        }
        Event::Print { step, depth, text, stderr: false } => {
            (json!({ "step": step, "depth": depth, "print": text }), *depth, format!("| {text}"))
        }
        Event::Print { step, depth, text, stderr: true } => {
            (json!({ "step": step, "depth": depth, "eprint": text }), *depth, format!("! {text}"))
        }
    }
}

/// Copies made at one site: (site, function, bytes, times, by the call they came from).
type Copied = (String, String, u64, u64, Vec<From>);

/// The copies that came from one call: (site, bytes, times, its arguments and why each is not
/// moved, None when it is).
type From = (String, u64, u64, Vec<(String, Option<&'static str>)>);

/// The copy sites shown in the text, and the calls shown for each.
const COPY_SITES: usize = 10;
const COPY_CALLS: usize = 3;

/// The source line a site (`file:line:col`) is on, trimmed; empty when it cannot be read.
fn site_line(files: &crate::Files, site: &str) -> String {
    let mut parts = site.rsplitn(3, ':');
    let (Some(_), Some(line), Some(file)) = (parts.next(), parts.next(), parts.next()) else { return String::new() };
    let (Ok(line), Ok(text)) = (line.parse::<usize>(), files.read(file)) else { return String::new() };
    let text = text.lines().nth(line.wrapping_sub(1)).unwrap_or("").trim();
    match text.char_indices().nth(80) {
        Some((cut, _)) => format!("{}...", &text[..cut]),
        None => text.to_string(),
    }
}

pub fn profile(root: &Path, file: &str, test: Option<&str>, max_steps: Option<u64>, input: &ProgramInput) -> Report {
    let (_, program) = try_report!(compile(root, file));
    let which = try_report!(target(&program, file, test));
    let given = try_report!(given(root, which, input));
    let run = run_name(&program, which);
    let make = |p: &Program| {
        let mut r = Recorder::new(p);
        r.sites = true;
        r
    };
    let observed = observe(program, which, max_steps, root, given, make, |p, rec, ended| {
        let mut ran: Vec<(String, u64, u64, u64, u64, u64, u64)> = Vec::new();
        let mut not_run = Vec::new();
        for (f, s) in p.funcs.iter().zip(&rec.funcs) {
            if s.calls == 0 {
                not_run.push(f.name.clone());
            } else {
                ran.push((f.name.clone(), s.calls, s.steps, s.self_steps, s.bytes, s.self_bytes, s.self_copied));
            }
        }
        // Most expensive first; the name breaks ties so the order never depends on anything else.
        ran.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
        let mut copies: Vec<Copied> = rec
            .copies
            .iter()
            .map(|(at, c)| {
                let func = c.func.map_or("a test".to_string(), |f| p.funcs[f].name.clone());
                let mut from: Vec<From> = c
                    .from
                    .iter()
                    .map(|(s, (b, t))| {
                        let args = p.handed.get(s).map_or(Vec::new(), |ks| ks.iter().map(|k| (k.what.clone(), k.kept)).collect());
                        (s.clone(), *b, *t, args)
                    })
                    .collect();
                from.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                (at.clone(), func, c.bytes, c.times, from)
            })
            .collect();
        copies.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        (ran, not_run, rec.steps, rec.bytes, rec.copied, copies, ended)
    });
    let (funcs, not_run, steps, bytes, copied, copies, ended) = try_report!(observed.map_err(Report::error));
    let files = crate::Files::disk(root);
    let mut text = format!(
        "{:>10} {:>10} {:>8} {:>11} {:>11} {:>11}  function ({run} in {file})\n",
        "self", "total", "calls", "self bytes", "copied", "bytes"
    );
    for (name, calls, total, own, b, own_b, own_c) in &funcs {
        text.push_str(&format!("{own:>10} {total:>10} {calls:>8} {own_b:>11} {own_c:>11} {b:>11}  {name}\n"));
    }
    text.push_str(&format!("{steps} steps, {bytes} bytes allocated; end: {}\n", ended.text()));
    if copied > 0 {
        text.push_str(&format!(
            "{copied} of those bytes copied values still shared when they changed; a variable, \
             field or element read for the last time is moved instead of shared. Where, costliest \
             first, and the calls that handed the shared values in:\n"
        ));
        for (at, func, b, times, from) in copies.iter().take(COPY_SITES) {
            text.push_str(&format!("{b:>12} bytes {times:>8}x  {at} in {func}: {}\n", site_line(&files, at)));
            // Copies in main's or a test's own body came from no call: nothing to add.
            let calls: Vec<&From> = from.iter().filter(|(site, ..)| !site.is_empty()).collect();
            for (site, b, times, args) in calls.iter().take(COPY_CALLS) {
                text.push_str(&format!("{b:>12} bytes {times:>8}x    from {site}: {}\n", site_line(&files, site)));
                for (what, kept) in args {
                    let note = match kept {
                        Some(why) => format!("`{what}` is not moved there: {why}"),
                        None => format!("`{what}` is moved there (what shares it is elsewhere: another value, or the callee)"),
                    };
                    text.push_str(&format!("{:>34}{note}\n", ""));
                }
            }
            if calls.len() > COPY_CALLS {
                text.push_str(&format!("{:>32}... {} more call site(s)\n", "", calls.len() - COPY_CALLS));
            }
        }
        if copies.len() > COPY_SITES {
            text.push_str(&format!("... {} more copy site(s) in --json\n", copies.len() - COPY_SITES));
        }
    }
    if !not_run.is_empty() {
        text.push_str(&format!("not run: {}\n", not_run.join(", ")));
    }
    let functions: Vec<Value> = funcs
        .iter()
        .map(|(name, calls, total, own, b, own_b, own_c)| {
            json!({
                "name": name, "calls": calls, "steps": total, "self_steps": own, "bytes": b, "self_bytes": own_b,
                "self_copied": own_c,
            })
        })
        .collect();
    let copies: Vec<Value> = copies
        .iter()
        .map(|(at, func, b, times, from)| {
            let from: Vec<Value> = from
                .iter()
                .map(|(site, b, times, args)| {
                    let args: Vec<Value> =
                        args.iter().map(|(what, kept)| json!({ "value": what, "moved": kept.is_none(), "kept": kept })).collect();
                    json!({ "at": site, "line": site_line(&files, site), "bytes": b, "times": times, "args": args })
                })
                .collect();
            json!({ "at": at, "line": site_line(&files, at), "function": func, "bytes": b, "times": times, "from": from })
        })
        .collect();
    let json = json!({
        "ok": true, "file": file, "run": run, "steps": steps, "bytes": bytes, "copied": copied, "functions": functions,
        "copies": copies, "not_run": not_run,
        "end": ended.to_json(),
    });
    Report::ok(json, text)
}
