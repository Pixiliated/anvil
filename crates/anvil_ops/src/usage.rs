//! `anvil usage`: the steps, bytes allocated and bytes copied of each run of a file (main under
//! its golden's input, and each test) against the numbers approved in `golden/FILE.usage`. The
//! counts come from the interpreter, so they are the same on every machine: any growth is a
//! change someone should look at, and `verify` reports it. Less is reported too, so the gain can
//! be approved and kept.

use crate::golden::{self, Counts};
use crate::project::Project;
use crate::testing::Ran;
use crate::{Files, Report, EXIT_FAILED, EXIT_OK};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What one run takes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Usage {
    /// `main`, or `test "name"`.
    pub run: String,
    pub counts: [u64; 3],
}

/// What `Usage::counts` holds, in order.
const UNITS: [&str; 3] = ["steps", "bytes", "copied"];

impl Usage {
    fn line(&self) -> String {
        let [s, b, c] = self.counts;
        format!("{}: {s} steps, {b} bytes, {c} copied", self.run)
    }

    fn parse(line: &str) -> Option<Usage> {
        let (run, rest) = line.rsplit_once(": ")?;
        let mut counts = [0; 3];
        let mut parts = rest.split(", ");
        for (c, unit) in counts.iter_mut().zip(UNITS) {
            let (n, u) = parts.next()?.split_once(' ')?;
            if u != unit {
                return None;
            }
            *c = n.parse().ok()?;
        }
        parts.next().is_none().then(|| Usage { run: run.to_string(), counts })
    }
}

/// The runs of a file that were measured, and those that exist but could not be (a failing test,
/// a main without a matching golden), which are left out of the comparison.
pub(crate) struct Measured {
    pub runs: Vec<Usage>,
    pub skipped: Vec<(String, String)>,
}

/// The runs measured from a file's tests and its golden run. `golden` is the golden check's
/// status and counts.
pub(crate) fn measured(tests: &[Ran], golden: (&str, Option<&Counts>)) -> Measured {
    let mut runs = Vec::new();
    let mut skipped = Vec::new();
    match golden {
        ("ok", Some(c)) => runs.push(Usage { run: "main".into(), counts: [c.steps, c.bytes, c.copied] }),
        ("no_main", _) => {}
        (status, _) => skipped.push(("main".into(), format!("its golden check is {status}"))),
    }
    for t in tests {
        let run = format!("test \"{}\"", t.name);
        match &t.failure {
            None => runs.push(Usage { run, counts: t.usage }),
            Some(_) => skipped.push((run, "it fails".into())),
        }
    }
    Measured { runs, skipped }
}

/// One difference from the approved usage.
pub(crate) struct Change {
    /// `more`, `less`, `new` or `gone`.
    pub kind: &'static str,
    pub text: String,
}

impl Change {
    /// Whether it must be looked at: anything but less.
    pub fn regression(&self) -> bool {
        self.kind != "less"
    }
}

fn changes(approved: &[Usage], now: &Measured) -> Vec<Change> {
    let mut out = Vec::new();
    for u in &now.runs {
        let Some(a) = approved.iter().find(|a| a.run == u.run) else {
            out.push(Change { kind: "new", text: format!("{} is not approved yet", u.line()) });
            continue;
        };
        for ((old, new), unit) in a.counts.iter().zip(&u.counts).zip(UNITS) {
            if old == new {
                continue;
            }
            let (kind, sign, d) = if new > old { ("more", '+', new - old) } else { ("less", '-', old - new) };
            let pct = if *old == 0 { String::new() } else { format!(", {sign}{:.1}%", d as f64 * 100.0 / *old as f64) };
            out.push(Change { kind, text: format!("{}: {old} -> {new} {unit} ({sign}{d}{pct})", u.run) });
        }
    }
    for a in approved {
        let measured = now.runs.iter().any(|u| u.run == a.run);
        let skipped = now.skipped.iter().any(|(r, _)| *r == a.run);
        if !measured && !skipped {
            out.push(Change { kind: "gone", text: format!("{}: approved, but it no longer runs", a.run) });
        }
    }
    out
}

pub(crate) fn path(project: &Project, file: &str) -> PathBuf {
    project.root.join(&project.golden_dir).join(format!("{file}.usage"))
}

fn approved(project: &Project, file: &str) -> Result<Option<Vec<Usage>>, String> {
    let p = path(project, file);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        match Usage::parse(line) {
            Some(u) => out.push(u),
            None => {
                return Err(format!(
                    "{}:{}: not a usage line (`RUN: N steps, N bytes, N copied`): {line}",
                    p.display(),
                    n + 1
                ))
            }
        }
    }
    Ok(Some(out))
}

/// A file's usage against its approved one: `none` (nothing runs, nothing approved), `ok`,
/// `less` (only less than approved), `missing` (runs, none approved) or `differs`.
pub(crate) struct Checked {
    pub file: String,
    pub measured: Measured,
    pub status: &'static str,
    pub changes: Vec<Change>,
}

pub(crate) fn check(project: &Project, file: &str, measured: Measured) -> Result<Checked, String> {
    let (status, changes) = match approved(project, file)? {
        None if measured.runs.is_empty() => ("none", Vec::new()),
        None => ("missing", Vec::new()),
        Some(a) => {
            let c = changes(&a, &measured);
            let status = match (c.is_empty(), c.iter().any(Change::regression)) {
                (true, _) => "ok",
                (false, false) => "less",
                (false, true) => "differs",
            };
            (status, c)
        }
    };
    Ok(Checked { file: file.to_string(), measured, status, changes })
}

impl Checked {
    pub fn failed(&self) -> bool {
        matches!(self.status, "missing" | "differs")
    }

    pub fn to_json(&self) -> Value {
        let runs: Vec<Value> = self
            .measured
            .runs
            .iter()
            .map(|u| json!({ "run": u.run, "steps": u.counts[0], "bytes": u.counts[1], "copied": u.counts[2] }))
            .collect();
        let skipped: Vec<Value> =
            self.measured.skipped.iter().map(|(r, why)| json!({ "run": r, "why": why })).collect();
        let changes: Vec<Value> = self.changes.iter().map(|c| json!({ "kind": c.kind, "text": c.text })).collect();
        json!({ "file": self.file, "status": self.status, "runs": runs, "skipped": skipped, "changes": changes })
    }

    /// The findings `verify` reports for it.
    pub fn findings(&self) -> Vec<String> {
        let f = &self.file;
        match self.status {
            // A run that cannot be measured (a failing test) is a finding of its own; usage is
            // approved once everything runs.
            "missing" if !self.measured.skipped.is_empty() => Vec::new(),
            "missing" => vec![format!(
                "{f} has no approved usage; check it with `anvil usage {f}`, then `anvil usage approve {f}`"
            )],
            "differs" => self
                .changes
                .iter()
                .filter(|c| c.regression())
                .map(|c| format!("usage {}; if that is meant, `anvil usage approve {f}`", c.text))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Runs a file's tests and its golden in the interpreter, as `verify` does, and measures them.
pub(crate) fn measure(project: &Project, file: &str, max_steps: Option<u64>) -> Result<Measured, String> {
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
    let mut ffi = crate::c_functions(&program)?;
    let p2 = program.clone();
    let root = project.root.clone();
    let tests = anvil_interp::run_on_big_stack(move || {
        let mut rec = anvil_interp::Recorder::new(&p2);
        crate::testing::run_all(&p2, crate::PROPERTY_CASES, 0, max_steps, Some(&mut rec), &mut ffi, &root)
    });
    let g = golden::check_file(project, &src, program, false, false, max_steps);
    Ok(measured(&tests, (g.status, g.counts.as_ref())))
}

fn files(root: &Path, file: Option<&str>) -> Result<(Project, Vec<String>), Report> {
    let project = Project::discover(root).map_err(Report::usage)?;
    let files = crate::proj::select(&project, root, file).map_err(Report::usage)?;
    Ok((project, files))
}

fn show(c: &Checked, text: &mut String, all: bool) {
    text.push_str(&format!("{} ({})\n", c.file, c.status));
    if all || c.status == "missing" {
        for u in &c.measured.runs {
            text.push_str(&format!("    {}\n", u.line()));
        }
    }
    for (run, why) in &c.measured.skipped {
        text.push_str(&format!("    {run}: not measured, as {why}\n"));
    }
    for ch in &c.changes {
        let note = match ch.kind {
            "more" => " (more than approved)",
            "less" => " (less: approve it to keep the gain)",
            _ => "",
        };
        text.push_str(&format!("  {}{note}\n", ch.text));
    }
    if c.status == "missing" {
        text.push_str(&format!("  no approved usage; check these, then `anvil usage approve {}`\n", c.file));
    }
}

/// Every project file's usage (or one file's) against its approved one.
pub fn usage(root: &Path, file: Option<&str>, max_steps: Option<u64>) -> Report {
    let (project, files) = match files(root, file) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let mut results = Vec::new();
    let mut text = String::new();
    for f in &files {
        let c = match measure(&project, f, max_steps).and_then(|m| check(&project, f, m)) {
            Ok(c) => c,
            Err(e) => return Report::usage(e),
        };
        if c.status == "none" && file.is_none() {
            continue;
        }
        if file.is_some() || c.status != "ok" {
            show(&c, &mut text, file.is_some());
        }
        results.push(c);
    }
    let bad = results.iter().filter(|c| c.failed()).count();
    text.push_str(&format!(
        "{} of {} file(s) use no more than their approved usage\n",
        results.len() - bad,
        results.len()
    ));
    let json = json!({ "ok": bad == 0, "files": results.iter().map(Checked::to_json).collect::<Vec<_>>() });
    let exit = if bad == 0 { EXIT_OK } else { EXIT_FAILED };
    Report { json, stdout: text, stderr: String::new(), exit, failed: bad > 0 }
}

/// Stores a file's current usage as approved (every project file's without one), or removes the
/// approved usage of a file where nothing runs any more. Refused while a run cannot be measured.
pub fn usage_approve(root: &Path, file: Option<&str>, max_steps: Option<u64>) -> Report {
    let (project, files) = match files(root, file) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let mut text = String::new();
    let mut done = Vec::new();
    for f in &files {
        let c = match measure(&project, f, max_steps).and_then(|m| check(&project, f, m)) {
            Ok(c) => c,
            Err(e) => return Report::usage(e),
        };
        if let Some((run, why)) = c.measured.skipped.first() {
            let m = format!("{f}: {run} cannot be measured, as {why}; fix that before approving its usage");
            if file.is_some() {
                return Report::usage(m);
            }
            text.push_str(&format!("not approved: {m}\n"));
            continue;
        }
        let p = path(&project, f);
        let what = match (c.status, c.measured.runs.is_empty()) {
            ("none", _) if file.is_some() => {
                return Report::usage(format!("{f} has no tests and no golden-checked main, so no usage to approve"))
            }
            ("none" | "ok", _) => continue,
            (_, true) => {
                if let Err(e) = std::fs::remove_file(&p) {
                    return Report::error(format!("cannot remove {}: {e}", p.display()));
                }
                "removed: nothing runs any more"
            }
            _ => {
                let body: String = c.measured.runs.iter().map(|u| format!("{}\n", u.line())).collect();
                let written = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, body));
                if let Err(e) = written {
                    return Report::error(format!("cannot write {}: {e}", p.display()));
                }
                if c.status == "missing" {
                    "new"
                } else {
                    "changed"
                }
            }
        };
        text.push_str(&format!("approved the usage of {f} ({what}):\n"));
        for u in &c.measured.runs {
            text.push_str(&format!("    {}\n", u.line()));
        }
        for ch in &c.changes {
            text.push_str(&format!("  {}\n", ch.text));
        }
        let mut json = c.to_json();
        json["approved"] = json!(what);
        json["path"] = json!(p.display().to_string());
        done.push(json);
    }
    if done.is_empty() {
        text.push_str("nothing to approve: every usage is already approved\n");
    }
    Report::ok(json!({ "ok": true, "approved": done }), text)
}
