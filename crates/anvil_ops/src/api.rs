//! A file's public API, approved like a program's output. `anvil api` prints it, `anvil api
//! approve FILE` stores it as `golden/FILE.api`, and verify reports every difference, saying
//! which ones break the files that use it. Docs are left out: they may change freely.

use crate::project::Project;
use crate::{Files, Report, EXIT_FAILED, EXIT_OK};
use anvil_front::ast::Item;
use anvil_front::span::{SourceFile, Span};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// One line per pub item, sorted, `pub` left off: what other files can use, with the names
/// they must write (calls name their arguments, so parameter names are part of it).
pub(crate) fn lines(src: &SourceFile) -> Result<Vec<String>, String> {
    let ast = anvil_front::parse(src).map_err(|d| format!("{} does not parse: {}", src.name, d[0].message))?;
    let text = |s: Span| crate::one_line(src.slice(s));
    // An item's text from its start up to the `{` of its body: the header with type parameters.
    let header = |start: usize, name: Span| {
        let brace = src.text[name.end..].find('{').map_or(src.text.len(), |i| name.end + i);
        crate::one_line(&src.text[start..brace])
    };
    let mut out = Vec::new();
    for item in &ast.items {
        let line = match item {
            Item::Fn(f) if f.public => text(Span::new(f.span.start, f.body.span.start)),
            Item::Struct(s) if s.public => {
                let fields: Vec<String> = s.fields.iter().map(|(f, t)| format!("{}: {}", f.name, text(t.span))).collect();
                format!("{} {{ {} }}", header(s.span.start, s.name.span), fields.join(", "))
            }
            Item::Enum(e) if e.public => {
                let variants: Vec<String> = e.variants.iter().map(|v| text(v.span)).collect();
                format!("{} {{ {} }}", header(e.span.start, e.name.span), variants.join(", "))
            }
            Item::Interface(i) if i.public => {
                let methods: Vec<String> = i.methods.iter().map(|m| text(m.span).trim_end_matches(';').to_string()).collect();
                format!("{} {{ {} }}", header(i.span.start, i.name.span), methods.join("; "))
            }
            Item::Unit(u) if u.public => text(u.span),
            Item::Const(c) if c.public => text(c.span),
            _ => continue,
        };
        out.push(line.strip_prefix("pub ").unwrap_or(&line).trim_end_matches(';').trim_end().to_string());
    }
    out.sort();
    Ok(out)
}

/// What an API line declares, so a changed signature is told from a removed item: its keyword
/// and name, type parameters left out (`fn Set.has`, `struct Pair`).
fn key(line: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '(' | '{' | ';' => break,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

/// One difference from the approved API; removed and changed items break their users.
pub(crate) struct Change {
    pub kind: &'static str,
    pub text: String,
}

impl Change {
    pub fn breaking(&self) -> bool {
        self.kind != "added"
    }
}

pub(crate) fn changes(approved: &[String], now: &[String]) -> Vec<Change> {
    let mut out = Vec::new();
    for a in approved {
        match now.iter().find(|n| key(n) == key(a)) {
            None => out.push(Change { kind: "removed", text: format!("removed `{a}`") }),
            Some(n) if n != a => out.push(Change { kind: "changed", text: format!("changed `{a}` to `{n}`") }),
            Some(_) => {}
        }
    }
    for n in now {
        if !approved.iter().any(|a| key(a) == key(n)) {
            out.push(Change { kind: "added", text: format!("added `{n}`") });
        }
    }
    out
}

pub(crate) fn path(project: &Project, file: &str) -> PathBuf {
    project.root.join(&project.golden_dir).join(format!("{file}.api"))
}

/// The approved API of a file, if one was approved.
pub(crate) fn approved(project: &Project, file: &str) -> Result<Option<Vec<String>>, String> {
    let p = path(project, file);
    match std::fs::read_to_string(&p) {
        Ok(t) => Ok(Some(t.lines().map(String::from).collect())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", p.display())),
    }
}

/// A file's API against its approved one: `none` (no pub items and none approved), `ok`,
/// `missing` (pub items, none approved) or `differs`, with the differences.
pub(crate) struct Checked {
    pub file: String,
    pub lines: Vec<String>,
    pub status: &'static str,
    pub changes: Vec<Change>,
}

pub(crate) fn check(project: &Project, file: &str) -> Result<Checked, String> {
    let src = Files::disk(&project.root).load(file).map_err(|r| r.json["error"].as_str().unwrap_or("").to_string())?;
    let now = lines(&src)?;
    let (status, changes) = match approved(project, file)? {
        None if now.is_empty() => ("none", Vec::new()),
        None => ("missing", Vec::new()),
        Some(a) => {
            let c = changes(&a, &now);
            (if c.is_empty() { "ok" } else { "differs" }, c)
        }
    };
    Ok(Checked { file: file.to_string(), lines: now, status, changes })
}

impl Checked {
    pub fn failed(&self) -> bool {
        matches!(self.status, "missing" | "differs")
    }

    pub fn to_json(&self) -> Value {
        let changes: Vec<Value> = self.changes.iter().map(|c| json!({ "kind": c.kind, "text": c.text })).collect();
        json!({ "file": self.file, "status": self.status, "api": self.lines, "changes": changes })
    }
}

/// Every project file's API (or one file's) against its approved one.
pub fn api(root: &Path, file: Option<&str>) -> Report {
    let project = match Project::discover(root) {
        Ok(p) => p,
        Err(e) => return Report::usage(e),
    };
    let files = match crate::proj::select(&project, root, file) {
        Ok(f) => f,
        Err(e) => return Report::usage(e),
    };
    let mut results = Vec::new();
    let mut text = String::new();
    for f in &files {
        let c = match check(&project, f) {
            Ok(c) => c,
            Err(e) => return Report::usage(e),
        };
        if c.status == "none" && file.is_none() {
            continue;
        }
        text.push_str(&format!("{} ({})\n", c.file, c.status));
        for l in &c.lines {
            text.push_str(&format!("    {l}\n"));
        }
        for ch in &c.changes {
            text.push_str(&format!("  {}{}\n", ch.text, if ch.breaking() { " (breaks its users)" } else { "" }));
        }
        if c.status == "missing" {
            text.push_str(&format!("  no approved API; check it, then `anvil api approve {}`\n", c.file));
        }
        results.push(c);
    }
    let bad = results.iter().filter(|c| c.failed()).count();
    text.push_str(&format!("{} of {} public API(s) match their approved ones\n", results.len() - bad, results.len()));
    let json = json!({ "ok": bad == 0, "files": results.iter().map(Checked::to_json).collect::<Vec<_>>() });
    let exit = if bad == 0 { EXIT_OK } else { EXIT_FAILED };
    Report { json, stdout: text, stderr: String::new(), exit, failed: bad > 0 }
}

/// Stores a file's current API as approved (or removes the approved one when it has none left).
pub fn api_approve(root: &Path, file: &str) -> Report {
    let project = match Project::discover(root) {
        Ok(p) => p,
        Err(e) => return Report::usage(e),
    };
    let rel = match crate::proj::select(&project, root, Some(file)) {
        Ok(mut f) => f.remove(0),
        Err(e) => return Report::usage(e),
    };
    let c = match check(&project, &rel) {
        Ok(c) => c,
        Err(e) => return Report::usage(e),
    };
    let p = path(&project, &rel);
    let what = match (c.status, c.lines.is_empty()) {
        ("none", _) => return Report::usage(format!("{rel} has no pub items, so no API to approve")),
        ("ok", _) => "unchanged",
        (_, true) => {
            if let Err(e) = std::fs::remove_file(&p) {
                return Report::error(format!("cannot remove {}: {e}", p.display()));
            }
            "removed: it has no pub items left"
        }
        _ => {
            let body: String = c.lines.iter().map(|l| format!("{l}\n")).collect();
            let written = p.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&p, body));
            if let Err(e) = written {
                return Report::error(format!("cannot write {}: {e}", p.display()));
            }
            if c.status == "missing" { "new" } else { "changed" }
        }
    };
    let mut text = format!("approved the API of {rel} ({what}):\n");
    for l in &c.lines {
        text.push_str(&format!("    {l}\n"));
    }
    for ch in &c.changes {
        text.push_str(&format!("  {}\n", ch.text));
    }
    let mut json = c.to_json();
    json["ok"] = json!(true);
    json["approved"] = json!(what);
    json["path"] = json!(p.display().to_string());
    Report::ok(json, text)
}
