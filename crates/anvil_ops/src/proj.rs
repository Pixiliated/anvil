//! Project-wide operations: everything that needs `anvil.toml`. The project is found from the
//! directory the command runs in (the CLI's current directory, the MCP server's root).

use crate::golden::{self, Checked};
use crate::project::Project;
use crate::{diagnostics, load, Report, EXIT_FAILED, EXIT_OK};
use anvil_record::{Record, Seen};
use serde_json::{json, Value};
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
    let ast = anvil_front::parse(&src).map_err(|d| format!("{}: {}", src.site(d.span), d.message))?;
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
            return Checked { file: file.into(), status: "error", detail: r.json["error"].as_str().map(String::from) }
        }
    };
    match anvil_front::compile(&src) {
        Ok(program) => golden::check_file(project, &src, program, native, max_steps),
        Err(d) => Checked {
            file: file.into(),
            status: "error",
            detail: Some(format!("{} compile error(s), first: {}", d.len(), d[0].message)),
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
    let program = match anvil_front::compile(&src) {
        Ok(p) => p,
        Err(d) => return diagnostics(&src, &d),
    };
    if program.main.is_none() {
        return Report::usage(format!("{rel} has no main, so it has no output to approve"));
    }
    let out = match golden::interpret(program, max_steps) {
        Ok(o) => o,
        Err(m) => return failed(json!({ "ok": false, "stopped": m }), format!("error: {m}\n")),
    };
    let previous = try_or_usage!(golden::read(&project, &rel));
    let change = match &previous {
        None => "new".to_string(),
        Some(p) => golden::difference(p, &out).unwrap_or_else(|| "unchanged".into()),
    };
    let path = try_or_usage!(golden::write(&project, &rel, &out));
    let mut json = out.to_json();
    json["ok"] = json!(true);
    json["golden"] = json!(path.display().to_string());
    json["change"] = json!(change);
    ok(json, format!("approved {rel} ({change}):\n{}", out.to_text()))
}

/// One verify pass over a file: compile errors, then failing tests, then the golden.
fn verify_file(project: &Project, file: &str, native: bool, max_steps: Option<u64>, seen: &mut Vec<Seen>) -> Value {
    let mut add = |check: &str, message: String, at: String| {
        seen.push(Seen { file: file.into(), check: check.into(), message, at });
    };
    let src = match load(&project.root, file) {
        Ok(s) => s,
        Err(r) => {
            let e = r.json["error"].as_str().unwrap_or("").to_string();
            add("read", e.clone(), String::new());
            return json!({ "file": file, "error": e });
        }
    };
    let program = match anvil_front::compile(&src) {
        Ok(p) => p,
        Err(diags) => {
            for d in &diags {
                let (l, c) = src.line_col(d.span.start);
                add(d.code, d.message.clone(), format!("{l}:{c}"));
            }
            return json!({ "file": file, "errors": diags.len() });
        }
    };
    let tests = program.tests.clone();
    let p2 = program.clone();
    let results = anvil_interp::run_on_big_stack(move || {
        (0..p2.tests.len()).map(|i| anvil_interp::run_test(&p2, i, &mut Vec::new(), max_steps)).collect::<Vec<_>>()
    });
    let mut test_failures = 0;
    for (t, r) in tests.iter().zip(&results) {
        if let Err(m) = r {
            test_failures += 1;
            add("test", format!("test \"{}\" fails: {m}", t.name), String::new());
        }
    }
    let g = golden::check_file(project, &src, program, native, max_steps);
    if g.failed() {
        add("golden", format!("{}: {}", g.status, g.detail.clone().unwrap_or_default()), String::new());
    }
    json!({ "file": file, "errors": 0, "tests": tests.len(), "tests_failed": test_failures, "golden": g.status })
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
    let files: Vec<Value> =
        project.files.iter().map(|f| verify_file(&project, f, native, max_steps, &mut seen)).collect();
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
