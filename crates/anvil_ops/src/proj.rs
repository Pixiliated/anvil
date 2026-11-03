//! Project-wide operations: everything that needs `anvil.toml`. The project is found from the
//! directory the command runs in (the CLI's current directory, the MCP server's root).

use crate::golden::{self, Checked, Counts};
use crate::project::Project;
use crate::{diagnostics, load, loader, Report, EXIT_FAILED, EXIT_OK};
use anvil_interp::Recorder;
use anvil_record::{Record, Seen};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

macro_rules! try_or_usage {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Report::usage(e),
        }
    };
}

fn ok(json: Value, text: String) -> Report {
    Report { json, stdout: text, stderr: String::new(), exit: EXIT_OK, failed: false }
}

fn failed(json: Value, text: String) -> Report {
    Report { json, stdout: text, stderr: String::new(), exit: EXIT_FAILED, failed: true }
}

/// What a project file holds, from its syntax alone.
struct Summary {
    main: bool,
    functions: Vec<String>,
    tests: usize,
}

fn summarize(project: &Project, file: &str) -> Result<Summary, String> {
    use anvil_front::ast::Item;
    let src = load(&project.root, file).map_err(|r| r.json["error"].as_str().unwrap_or("").to_string())?;
    let ast = anvil_front::parse(&src).map_err(|d| format!("{}: {}", src.site(d[0].span), d[0].message))?;
    let mut s = Summary { main: false, functions: Vec::new(), tests: 0 };
    for item in &ast.items {
        match item {
            Item::Fn(f) => {
                s.main |= f.name.name == "main";
                let sig = src.slice(anvil_front::span::Span::new(f.span.start, f.body.span.start));
                s.functions.push(sig.split_whitespace().collect::<Vec<_>>().join(" "));
            }
            Item::Test(_) => s.tests += 1,
            _ => {}
        }
    }
    Ok(s)
}

pub fn list(root: &Path) -> Report {
    let project = try_or_usage!(Project::discover(root));
    let mut files = Vec::new();
    let mut text = format!("{}: {} file(s)\n", project.name, project.files.len());
    for f in &project.files {
        let golden = golden::path(&project, f).is_file();
        match summarize(&project, f) {
            Ok(s) => {
                text.push_str(&format!(
                    "  {f}  {} fn, {} test(s){}{}\n",
                    s.functions.len(),
                    s.tests,
                    if s.main { ", main" } else { "" },
                    if golden { ", golden" } else { "" }
                ));
                files.push(json!({ "file": f, "main": s.main, "functions": s.functions.len(), "tests": s.tests, "golden": golden }));
            }
            Err(e) => {
                text.push_str(&format!("  {f}  does not parse: {e}\n"));
                files.push(json!({ "file": f, "error": e }));
            }
        }
    }
    ok(json!({ "ok": true, "project": project.name, "root": project.root.display().to_string(), "files": files }), text)
}

/// The project-relative names of `file` (when given) or of every project file.
fn select(project: &Project, cwd: &Path, file: Option<&str>) -> Result<Vec<String>, String> {
    match file {
        None => Ok(project.files.clone()),
        Some(f) => {
            let rel = project.relative(cwd, f)?;
            if !project.contains(&rel) {
                return Err(format!(
                    "{rel} is not one of the project's files; add it to [project] files in anvil.toml"
                ));
            }
            Ok(vec![rel])
        }
    }
}

fn check_one(project: &Project, file: &str, native: bool, max_steps: Option<u64>) -> Checked {
    let src = match load(&project.root, file) {
        Ok(s) => s,
        Err(r) => {
            return Checked {
                file: file.into(),
                status: "error",
                detail: r.json["error"].as_str().map(String::from),
                counts: None,
            }
        }
    };
    match anvil_front::compile_with(&src, &loader(&project.root)).1 {
        Ok(program) => golden::check_file(project, &src, program, native, max_steps),
        Err(d) => Checked {
            file: file.into(),
            status: "error",
            detail: Some(format!("{} compile error(s), first: {}", d.len(), d[0].message)),
            counts: None,
        },
    }
}

pub fn golden_check(root: &Path, file: Option<&str>, native: bool, max_steps: Option<u64>) -> Report {
    let project = try_or_usage!(Project::discover(root));
    let files = try_or_usage!(select(&project, root, file));
    let results: Vec<Checked> = files.iter().map(|f| check_one(&project, f, native, max_steps)).collect();
    let bad: Vec<&Checked> = results.iter().filter(|c| c.failed()).collect();
    let checked = results.iter().filter(|c| c.status != "no_main").count();
    let mut text = String::new();
    for c in &bad {
        text.push_str(&format!("{} {}: {}\n", c.status.to_uppercase(), c.file, c.detail.as_deref().unwrap_or("")));
    }
    let backends = if native { "interpreter and native" } else { "interpreter" };
    text.push_str(&format!("{} of {checked} program(s) match their goldens ({backends})\n", checked - bad.len()));
    let json = json!({
        "ok": bad.is_empty(), "native": native, "checked": checked, "failed": bad.len(),
        "results": results.iter().map(Checked::to_json).collect::<Vec<_>>(),
    });
    if bad.is_empty() {
        ok(json, text)
    } else {
        failed(json, text)
    }
}

pub fn golden_approve(root: &Path, file: &str, max_steps: Option<u64>) -> Report {
    let project = try_or_usage!(Project::discover(root));
    let rel = try_or_usage!(select(&project, root, Some(file))).remove(0);
    let src = match load(&project.root, &rel) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let program = match anvil_front::compile_with(&src, &loader(&project.root)) {
        (_, Ok(p)) => p,
        (sources, Err(d)) => return diagnostics(&sources, &d),
    };
    if program.main.is_none() {
        return Report::usage(format!("{rel} has no main, so it has no output to approve"));
    }
    let input = try_or_usage!(golden::read_input(&project, &rel));
    let out = match golden::interpret(program, max_steps, &project.root, &input) {
        Ok((o, _)) => o,
        Err(m) => return failed(json!({ "ok": false, "stopped": m }), format!("error: {m}\n")),
    };
    let previous = try_or_usage!(golden::read(&project, &rel));
    let change = match &previous {
        None => "new".to_string(),
        Some(p) => golden::difference(p, &out).unwrap_or_else(|| "unchanged".into()),
    };
    let path = try_or_usage!(golden::write(&project, &rel, &out));
    golden::clear_actual(&project, &rel);
    let mut json = out.to_json();
    json["ok"] = json!(true);
    json["golden"] = json!(path.display().to_string());
    json["change"] = json!(change);
    ok(json, format!("approved {rel} ({change}):\n{}", out.to_text()))
}

/// Where a function or branch is declared: its file and the offset of its name (of its block's
/// `{`, for a branch), the same from every file that imports it.
type Key = (String, usize);

/// What one file's runs covered, for the project-wide `untested` and `unreached` lints.
struct Coverage {
    file: String,
    /// The file's own functions; main is left to its golden.
    own: Vec<Own>,
    /// The key of every function that ran, imported ones too.
    ran: HashSet<Key>,
    /// The file's own branches.
    branches: Vec<Site>,
    /// The key of every branch that ran, imported ones too.
    reached: HashSet<Key>,
}

struct Own {
    name: String,
    key: Key,
    /// line:col of its name.
    at: String,
    allows_untested: bool,
    allows_unreached: bool,
}

/// A branch: its key, its function's key, line:col of its `{`, and what it is.
struct Site {
    key: Key,
    owner: Key,
    at: String,
    what: String,
}

/// One verify pass over a file: compile errors (in it or in a module it imports), then failing
/// tests, then the golden. What ran is returned for the `untested` lint, which needs every file.
fn verify_file(
    project: &Project,
    file: &str,
    native: bool,
    max_steps: Option<u64>,
    seen: &mut Vec<Seen>,
) -> (Value, Option<Coverage>) {
    let mut add = |check: &str, message: String, at: String| {
        seen.push(Seen { file: file.into(), check: check.into(), message, at });
    };
    let src = match load(&project.root, file) {
        Ok(s) => s,
        Err(r) => {
            let e = r.json["error"].as_str().unwrap_or("").to_string();
            add("read", e.clone(), String::new());
            return (json!({ "file": file, "error": e }), None);
        }
    };
    let program = match anvil_front::compile_with(&src, &loader(&project.root)) {
        (_, Ok(p)) => p,
        (sources, Err(diags)) => {
            for d in &diags {
                let at = &sources[d.file];
                let (l, c) = at.line_col(d.span.start);
                let place = if d.file == 0 { format!("{l}:{c}") } else { format!("{}:{l}:{c}", at.name) };
                add(d.code, d.message.clone(), place);
            }
            return (json!({ "file": file, "errors": diags.len() }), None);
        }
    };
    let tests = program.tests.clone();
    let funcs = program.funcs.clone();
    let modules = program.modules.clone();
    let generic_fns = program.generic_fns.clone();
    let program_main = program.main;
    let p2 = program.clone();
    let root2 = project.root.clone();
    let mut ffi = match crate::c_functions(&program) {
        Ok(f) => f,
        Err(e) => {
            add("build", format!("its C functions do not build: {e}"), String::new());
            None
        }
    };
    let branches = program.branches.clone();
    let (results, mut ran, mut reached) = anvil_interp::run_on_big_stack(move || {
        let mut ran = vec![false; p2.funcs.len()];
        let mut reached = vec![false; p2.branches.len()];
        let mut rec = Recorder::new(&p2);
        let results =
            crate::testing::run_all(&p2, crate::PROPERTY_CASES, 0, max_steps, Some(&mut rec), &mut ffi, &root2);
        mark_ran(&mut ran, &mut reached, &Counts::of(&rec));
        (results, ran, reached)
    });
    let mut test_failures = 0;
    for r in &results {
        if let Some(m) = &r.failure {
            test_failures += 1;
            let inputs = r.inputs.as_ref().map(|i| format!(" for {i}")).unwrap_or_default();
            add("test", format!("test \"{}\" fails{inputs}: {m}", r.name), String::new());
        }
    }
    let g = golden::check_file(project, &src, program, native, max_steps);
    if g.failed() {
        add("golden", format!("{}: {}", g.status, g.detail.clone().unwrap_or_default()), String::new());
    }
    if let Some(c) = &g.counts {
        mark_ran(&mut ran, &mut reached, c);
    }
    // main runs only under its golden, and a missing golden is already a finding.
    let main = g.status != "no_main";
    let key = |f: &anvil_front::ir::Func| (modules[f.module].clone(), f.span.start);
    let mut own = Vec::new();
    for (i, f) in funcs.iter().enumerate() {
        // An instance runs for its generic, which is listed below; a function literal is part
        // of the function it is written in.
        if f.module != 0 || f.instance || f.captures.is_some() || (main && program_main == Some(i)) {
            continue;
        }
        let (l, c) = src.line_col(f.span.start);
        own.push(Own {
            name: f.name.clone(),
            key: key(f),
            at: format!("{l}:{c}"),
            allows_untested: allows(&f.allows, "untested"),
            allows_unreached: allows(&f.allows, "unreached"),
        });
    }
    // Generic functions, called or not: one that no test or main instantiates and runs is untested.
    for g in generic_fns.iter().filter(|g| g.module == 0) {
        let (l, c) = src.line_col(g.span.start);
        own.push(Own {
            name: g.name.clone(),
            key: (modules[0].clone(), g.span.start),
            at: format!("{l}:{c}"),
            allows_untested: allows(&g.allows, "untested"),
            allows_unreached: allows(&g.allows, "unreached"),
        });
    }
    let ran = funcs.iter().zip(&ran).filter(|(_, r)| **r).map(|(f, _)| key(f)).collect();
    let site_key = |b: &anvil_front::ir::Branch| (modules[b.module].clone(), b.at.start);
    let own_branches: Vec<Site> = branches
        .iter()
        .filter(|b| b.module == 0)
        .map(|b| {
            let (l, c) = src.line_col(b.at.start);
            Site {
                key: site_key(b),
                owner: (modules[0].clone(), b.owner.start),
                at: format!("{l}:{c}"),
                what: b.kind.describe(),
            }
        })
        .collect();
    let reached = branches.iter().zip(&reached).filter(|(_, r)| **r).map(|(b, _)| site_key(b)).collect();
    let steps = g.counts.as_ref().map(|c| c.steps);
    let json = json!({
        "file": file, "errors": 0, "tests": tests.len(), "tests_failed": test_failures, "golden": g.status,
        "steps": steps, "untested": [], "unreached": [],
    });
    (json, Some(Coverage { file: file.to_string(), own, ran, branches: own_branches, reached }))
}

fn allows(list: &[(String, String)], lint: &str) -> bool {
    list.iter().any(|(l, _)| l == lint)
}

/// The `untested` lint over the whole project: a function counts as tested when a test or a
/// golden-checked main of any project file runs it, so a module is covered by its importers.
fn untested(coverage: &[Coverage], files: &mut [Value], seen: &mut Vec<Seen>) {
    let ran: HashSet<&Key> = coverage.iter().flat_map(|c| &c.ran).collect();
    for c in coverage {
        let mut names = Vec::new();
        for Own { name, key, at, allows_untested: allowed, .. } in &c.own {
            let mut add = |check: &str, message: String| {
                seen.push(Seen { file: c.file.clone(), check: check.into(), message, at: at.clone() });
            };
            match (ran.contains(key), allowed) {
                (false, false) => {
                    names.push(name.clone());
                    add("untested", format!("`{name}` never runs in a test or a golden-checked main; test it, or write `allow untested \"why\"` before it"));
                }
                (true, true) => add(
                    "allow",
                    format!(
                        "`{name}` runs in a test or a golden-checked main, so its `allow untested` is stale: remove it"
                    ),
                ),
                _ => {}
            }
        }
        if let Some(v) = files.iter_mut().find(|v| v["file"] == c.file.as_str()) {
            v["untested"] = json!(names);
        }
    }
}

/// The `unreached` lint over the whole project: a branch of a function that runs, which no test
/// or golden-checked main of any project file takes. A function that never runs is `untested`
/// instead, so its branches are not reported twice.
fn unreached(coverage: &[Coverage], files: &mut [Value], seen: &mut Vec<Seen>) {
    let ran: HashSet<&Key> = coverage.iter().flat_map(|c| &c.ran).collect();
    let reached: HashSet<&Key> = coverage.iter().flat_map(|c| &c.reached).collect();
    for c in coverage {
        let mut list = Vec::new();
        for f in c.own.iter().filter(|f| ran.contains(&f.key)) {
            let missed: Vec<&Site> =
                c.branches.iter().filter(|b| b.owner == f.key && !reached.contains(&b.key)).collect();
            if f.allows_unreached {
                if missed.is_empty() {
                    let message = format!(
                        "every branch of `{}` runs in a test or a golden-checked main, so its `allow unreached` is stale: remove it",
                        f.name
                    );
                    seen.push(Seen { file: c.file.clone(), check: "allow".into(), message, at: f.at.clone() });
                }
                continue;
            }
            for b in missed {
                let message = format!(
                    "{} in `{}` never runs in a test or a golden-checked main; take it in a test, or write `allow unreached \"why\"` before the function",
                    b.what, f.name
                );
                seen.push(Seen { file: c.file.clone(), check: "unreached".into(), message, at: b.at.clone() });
                list.push(json!({ "function": f.name, "at": b.at, "branch": b.what }));
            }
        }
        if let Some(v) = files.iter_mut().find(|v| v["file"] == c.file.as_str()) {
            let total = c.branches.len();
            let taken = c.branches.iter().filter(|b| reached.contains(&b.key)).count();
            v["unreached"] = json!(list);
            v["branches"] = json!({ "total": total, "taken": taken });
        }
    }
}

fn mark_ran(ran: &mut [bool], reached: &mut [bool], counts: &Counts) {
    for (r, f) in ran.iter_mut().zip(&counts.funcs) {
        *r |= f.calls > 0;
    }
    for (r, n) in reached.iter_mut().zip(&counts.branches) {
        *r |= *n > 0;
    }
}

struct Verified {
    project: Project,
    files: Vec<Value>,
    open: Vec<Value>,
    json: Value,
    passed: bool,
}

fn run_verify(root: &Path, native: bool, max_steps: Option<u64>) -> Result<Verified, String> {
    let project = Project::discover(root)?;
    let mut seen = Vec::new();
    let mut coverage = Vec::new();
    let mut files: Vec<Value> = Vec::new();
    for f in &project.files {
        let (v, c) = verify_file(&project, f, native, max_steps, &mut seen);
        files.push(v);
        coverage.extend(c);
    }
    untested(&coverage, &mut files, &mut seen);
    unreached(&coverage, &mut files, &mut seen);
    let mut record = Record::open(&project.root)?;
    let now = anvil_record::now_utc();
    let sync = record.sync(&seen, &now)?;
    let run = record.add_run(&now, files.len(), native, seen.len(), &sync)?;
    let open = record.findings(true)?;
    let json = json!({
        "ok": seen.is_empty(), "project": project.name, "native": native, "run": run, "files": files,
        "findings": { "open": open.len(), "opened": sync.opened, "resolved": sync.resolved },
        "open": open, "record": record.path().display().to_string(),
    });
    Ok(Verified { project, files, passed: seen.is_empty(), open, json })
}

pub fn verify(root: &Path, native: bool, max_steps: Option<u64>) -> Report {
    let v = try_or_usage!(run_verify(root, native, max_steps));
    let mut text = String::new();
    for f in &v.open {
        text.push_str(&format!(
            "#{} {}{} [{}] {}\n",
            f["id"],
            f["file"].as_str().unwrap(),
            match f["at"].as_str().unwrap() {
                "" => String::new(),
                at => format!(":{at}"),
            },
            f["check"].as_str().unwrap(),
            f["message"].as_str().unwrap()
        ));
    }
    let o = &v.json["findings"];
    text.push_str(&format!(
        "{}: {} file(s), {} open finding(s) ({} new, {} resolved)\n",
        v.project.name,
        v.files.len(),
        v.open.len(),
        o["opened"].as_array().unwrap().len(),
        o["resolved"].as_array().unwrap().len()
    ));
    if v.passed {
        ok(v.json, text)
    } else {
        failed(v.json, text)
    }
}

/// Runs verify, then writes `out/review.md`: the sheet a person reads before signing off.
pub fn review(root: &Path, native: bool, max_steps: Option<u64>) -> Report {
    let v = try_or_usage!(run_verify(root, native, max_steps));
    let record = try_or_usage!(Record::open(&v.project.root));
    let summary = try_or_usage!(record.summary());
    let mut md = format!(
        "# {}: review sheet\n\nGenerated {} by anvil {} ({}). Everything below was checked just now; \
         the boxes at the end are for the person signing off.\n\n## Summary\n\n",
        v.project.name,
        anvil_record::today_utc(),
        env!("CARGO_PKG_VERSION"),
        if native { "interpreter and native" } else { "interpreter only; add --native to check executables too" }
    );
    let count = |key: &str, val: &str| v.files.iter().filter(|f| f[key] == val).count();
    let tests: i64 = v.files.iter().filter_map(|f| f["tests"].as_i64()).sum();
    let failing: i64 = v.files.iter().filter_map(|f| f["tests_failed"].as_i64()).sum();
    let errors = v.files.iter().filter(|f| f["errors"].as_i64().unwrap_or(1) > 0).count();
    md.push_str(&format!("- {} file(s); {errors} with compile errors\n", v.files.len()));
    md.push_str(&format!("- {tests} test(s), {failing} failing\n"));
    md.push_str(&format!(
        "- goldens: {} match, {} missing, {} differ\n",
        count("golden", "ok"),
        count("golden", "missing"),
        count("golden", "differs")
    ));
    let branches = |key: &str| v.files.iter().filter_map(|f| f["branches"][key].as_u64()).sum::<u64>();
    md.push_str(&format!("- branches: {} of {} taken by a test or a golden\n", branches("taken"), branches("total")));
    md.push_str(&format!("- open findings: {}\n", v.open.len()));
    if let Some(c) = summary["current"].as_object() {
        md.push_str(&format!("- current milestone: {} {}\n", c["id"].as_str().unwrap(), c["title"].as_str().unwrap()));
    }
    md.push_str("\n## Open findings\n\n");
    if v.open.is_empty() {
        md.push_str("None.\n");
    }
    for f in &v.open {
        md.push_str(&format!(
            "- #{} `{}` {} [{}] {} (since {})\n",
            f["id"],
            f["file"].as_str().unwrap(),
            f["at"].as_str().unwrap(),
            f["check"].as_str().unwrap(),
            f["message"].as_str().unwrap(),
            f["first_seen"].as_str().unwrap()
        ));
    }
    md.push_str("\n## Files\n");
    for f in &v.files {
        let name = f["file"].as_str().unwrap();
        md.push_str(&format!("\n### `{name}`\n\n"));
        match summarize(&v.project, name) {
            Ok(s) => {
                let golden = f["golden"].as_str().unwrap_or("not checked (compile errors)");
                md.push_str(&format!(
                    "- tests: {} ({} failing); golden: {golden}\n",
                    s.tests,
                    f["tests_failed"].as_i64().unwrap_or(0)
                ));
                for sig in &s.functions {
                    md.push_str(&format!("- `{sig}`\n"));
                }
            }
            Err(e) => md.push_str(&format!("- does not parse: {e}\n")),
        }
    }
    md.push_str("\n## Recent decisions\n\n");
    let decisions = try_or_usage!(record.decisions());
    if decisions.is_empty() {
        md.push_str("None recorded (`anvil record decide`).\n");
    }
    for d in decisions.iter().rev().take(10) {
        md.push_str(&format!(
            "- **{} · {}.** {}\n",
            d["date"].as_str().unwrap(),
            d["title"].as_str().unwrap(),
            d["body"].as_str().unwrap()
        ));
    }
    md.push_str(
        "\n## Sign-off\n\n- [ ] I read the open findings and agree with each\n- [ ] I read the output of every new or \
         changed golden\n- [ ] The decisions above match what I asked for\n\nSigned: ______________  Date: __________\n",
    );
    let path = v.project.root.join("out").join("review.md");
    if let Err(e) = std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, &md)) {
        return Report::usage(format!("cannot write {}: {e}", path.display()));
    }
    let json = json!({ "ok": v.passed, "sheet": path.display().to_string(), "markdown": md, "verify": v.json });
    let text = format!("wrote {}\n{} open finding(s)\n", path.display(), v.open.len());
    if v.passed {
        ok(json, text)
    } else {
        failed(json, text)
    }
}

fn open_record(root: &Path) -> Result<(Project, Record), String> {
    let project = Project::discover(root)?;
    let record = Record::open(&project.root)?;
    Ok((project, record))
}

pub fn record_query(root: &Path, kind: Option<&str>, open_only: bool) -> Report {
    let (_, record) = try_or_usage!(open_record(root));
    let v = try_or_usage!(record.query(kind, open_only));
    let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
    ok(v, text)
}

pub fn record_decide(root: &Path, title: &str, body: Option<&str>) -> Report {
    let (_, record) = try_or_usage!(open_record(root));
    let v = try_or_usage!(record.decide(&anvil_record::today_utc(), title, body.unwrap_or("")));
    ok(v.clone(), format!("decision #{} recorded\n", v["id"]))
}

pub fn record_status(root: &Path, milestone: &str, status: &str) -> Report {
    let (_, record) = try_or_usage!(open_record(root));
    let v = try_or_usage!(record.set_status(milestone, status, &anvil_record::now_utc()));
    ok(v, format!("{milestone} is now {status}\n"))
}

pub fn record_import_roadmap(root: &Path, file: Option<&str>) -> Report {
    let (project, mut record) = try_or_usage!(open_record(root));
    let path = match file {
        Some(f) => root.join(f),
        None => project.root.join("docs").join("ROADMAP.md"),
    };
    let text =
        try_or_usage!(std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display())));
    let v = try_or_usage!(record.import_roadmap(&text, &anvil_record::now_utc()));
    let current =
        v["current"].as_object().map(|c| format!("{} {}", c["id"].as_str().unwrap(), c["title"].as_str().unwrap()));
    let msg = format!(
        "imported {} milestone item(s) from {}; current: {}\n",
        v["milestones"],
        path.display(),
        current.unwrap_or_else(|| "none (all done)".into())
    );
    ok(v, msg)
}

pub fn record_export(root: &Path) -> Report {
    let (project, record) = try_or_usage!(open_record(root));
    let md = try_or_usage!(record.export_markdown(&project.name));
    ok(json!({ "ok": true, "markdown": md }), md)
}

/// What watch compares between passes: every file verify reads, with its modification time and
/// size (None when missing). A file added to or removed from the project changes the list itself.
fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, Option<(std::time::SystemTime, u64)>)> {
    let paths = match Project::discover(root) {
        Ok(p) => {
            let mut paths = vec![p.root.join("anvil.toml")];
            for f in &p.files {
                paths.push(p.root.join(f));
                paths.push(golden::path(&p, f));
            }
            paths
        }
        Err(_) => vec![root.join("anvil.toml")],
    };
    paths
        .into_iter()
        .map(|p| {
            let stamp = std::fs::metadata(&p).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
            (p, stamp)
        })
        .collect()
}

/// `verify`, then again after every change to anvil.toml, a project file or a golden, handing
/// each pass's report to `emit` with whether another pass will follow. Runs until stopped, or
/// for `runs` passes; the returned report is the last pass's verdict. CLI only (see `CLI_ONLY_COMMANDS`).
pub fn watch(
    root: &Path,
    native: bool,
    max_steps: Option<u64>,
    runs: Option<u64>,
    emit: &mut dyn FnMut(&Report, bool),
) -> Report {
    if runs == Some(0) {
        return Report::usage("--runs must be at least 1");
    }
    let mut done = 0;
    loop {
        let before = snapshot(root);
        let report = verify(root, native, max_steps);
        done += 1;
        let last = runs.is_some_and(|n| done >= n);
        emit(&report, !last);
        if last {
            let json = json!({ "ok": !report.failed, "runs": done });
            return if report.failed { failed(json, String::new()) } else { ok(json, String::new()) };
        }
        while snapshot(root) == before {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

/// Puts files in canonical layout: `file` (no project needed), or every file of the project.
/// With `check`, writes nothing and fails if any file is not formatted.
pub fn fmt(root: &Path, file: Option<&str>, check: bool) -> Report {
    let (base, files) = match file {
        Some(f) => (root.to_path_buf(), vec![f.to_string()]),
        None => {
            let project = try_or_usage!(Project::discover(root));
            (project.root.clone(), project.files.clone())
        }
    };
    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut text = String::new();
    for f in &files {
        let src = match load(&base, f) {
            Ok(s) => s,
            Err(r) => return r,
        };
        match anvil_front::fmt::format(&src) {
            Ok(formatted) => {
                let changed = formatted != src.text;
                if changed && !check {
                    if let Err(e) = std::fs::write(base.join(f), &formatted) {
                        return Report::usage(format!("cannot write {f}: {e}"));
                    }
                }
                if changed {
                    text.push_str(&format!("{} {f}\n", if check { "not formatted:" } else { "formatted" }));
                }
                results.push(json!({ "file": f, "changed": changed }));
            }
            Err(d) => {
                text.push_str(&anvil_front::diag::render(&src, &d));
                text.push('\n');
                errors.push(anvil_front::diag::to_json(&src, &d));
            }
        }
    }
    let changed = results.iter().filter(|r| r["changed"] == true).count();
    let verb = if check { "not formatted" } else { "changed" };
    text.push_str(&format!("{} file(s), {changed} {verb}, {} could not be formatted\n", files.len(), errors.len()));
    let passed = errors.is_empty() && !(check && changed > 0);
    let json = json!({ "ok": passed, "check": check, "files": results, "diagnostics": errors });
    if passed {
        ok(json, text)
    } else {
        failed(json, text)
    }
}
