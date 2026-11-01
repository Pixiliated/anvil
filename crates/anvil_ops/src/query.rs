//! Questions about a position or a name, answered from the checker's facts: `type-at`, `refs`
//! and `complete`. They answer for files with type errors, and read through `Files`, so an
//! editor's unsaved text gets the same answers as the file on disk.

use crate::{diagnostics, Files, Report, EXIT_FAILED};
use anvil_front::check::{Analysis, Def, Loc, BUILTINS, BUILTIN_SIGNATURES, CONSTRUCTORS, METHODS, TYPE_NAMES};
use anvil_front::lexer::KEYWORDS;
use anvil_front::span::{SourceFile, Span};
use serde_json::{json, Value};

macro_rules! try_report {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}

fn failed(json: Value, text: String) -> Report {
    Report { json, stdout: String::new(), stderr: text, exit: EXIT_FAILED, failed: true }
}

fn usage(message: String) -> Report {
    Report::usage(message)
}

/// Checker facts for the root, and how many of its items were left out to get them.
fn analyze(files: Files, file: &str) -> Result<(Vec<SourceFile>, Analysis, usize), Report> {
    let src = files.load(file)?;
    learn(files, &src, None)
}

/// Lines where an item starts: canonical layout puts every item at column 1.
fn item_starts(text: &str) -> Vec<usize> {
    const ITEMS: &[&str] = &["fn ", "pub ", "struct ", "enum ", "unit ", "test ", "allow ", "import "];
    let mut starts = vec![0];
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if at > 0 && ITEMS.iter().any(|k| line.starts_with(k)) {
            starts.push(at);
        }
        at += line.len();
    }
    starts
}

/// Checker facts for `src`, or the syntax errors that prevent them. A syntax error stops the
/// checker, so each item of the root that does not parse is blanked out (keeping every offset)
/// and the rest is checked, unless the item holds offset `keep`: facts are wanted there.
fn learn(files: Files, src: &SourceFile, keep: Option<usize>) -> Result<(Vec<SourceFile>, Analysis, usize), Report> {
    let (sources, a) = anvil_front::analyze_with(src, &files.loader());
    if !(a.facts.defs.is_empty() && !a.diags.is_empty()) {
        return Ok((sources, a, 0));
    }
    let starts = item_starts(&src.text);
    let item_of = |o: usize| {
        let i = starts.partition_point(|s| *s <= o);
        (starts[i - 1], starts.get(i).copied().unwrap_or(src.text.len()))
    };
    let holds_keep =
        |item: (usize, usize)| keep.is_some_and(|k| item.0 <= k && (k < item.1 || item.1 == src.text.len()));
    // Errors that blanking cannot get past: in another file, or in the item that must stay.
    let stuck: Vec<_> = a.diags.iter().filter(|d| d.file != 0 || holds_keep(item_of(d.span.start))).cloned().collect();
    if !stuck.is_empty() {
        return Err(diagnostics(&sources, &stuck));
    }
    let mut blank = Vec::new();
    for d in &a.diags {
        let item = item_of(d.span.start);
        if !blank.contains(&item) {
            blank.push(item);
        }
    }
    let mut text = src.text.clone().into_bytes();
    for (start, end) in &blank {
        for b in &mut text[*start..*end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    let probe = SourceFile::new(src.name.clone(), String::from_utf8(text).expect("whole lines blanked"));
    let (mut sources, a2) = anvil_front::analyze_with(&probe, &files.loader());
    if a2.facts.defs.is_empty() && !a2.diags.is_empty() {
        return Err(diagnostics(&sources, &a.diags));
    }
    // Answers quote the real text; the offsets are the same.
    sources[0] = src.clone();
    Ok((sources, a2, blank.len()))
}

/// How many errors the answer was found despite, and a line saying so.
fn errors_note(a: &Analysis, skipped: usize) -> (usize, String) {
    let n = a.diags.len() + skipped;
    let mut note = String::new();
    if skipped > 0 {
        note.push_str(&format!("({skipped} item(s) with syntax errors were left out)\n"));
    }
    if !a.diags.is_empty() {
        note.push_str(&format!("(the file has {} error(s); answers may be incomplete)\n", a.diags.len()));
    }
    (n, note)
}

/// The innermost entry whose span contains `offset` (ends inclusive, so the end of a name counts).
fn innermost<T>(entries: impl Iterator<Item = (Span, T)>, offset: usize) -> Option<(Span, T)> {
    entries.filter(|(s, _)| s.start <= offset && offset <= s.end).min_by_key(|(s, _)| s.end - s.start)
}

/// A span as JSON: its start line and column and the column where it ends (names are one line).
fn place(src: &SourceFile, span: Span) -> Value {
    let (line, col) = src.line_col(span.start);
    let (_, end_col) = src.line_col(span.end);
    json!({ "line": line, "col": col, "end_col": end_col })
}

fn def_json(sources: &[SourceFile], d: &Def) -> Value {
    let src = &sources[d.file];
    json!({ "name": d.name, "kind": d.kind, "detail": d.detail, "file": src.name, "defined_at": place(src, d.span) })
}

fn find_def(a: &Analysis, loc: Loc) -> Option<&Def> {
    a.facts.defs.iter().find(|d| d.loc() == loc)
}

/// The declaration named at `offset` of the root: one declared there, or one used there.
fn named_at(a: &Analysis, offset: usize) -> Option<&Def> {
    let f = &a.facts;
    let declared = innermost(f.defs.iter().filter(|d| d.file == 0).map(|d| (d.span, d.loc())), offset);
    let (_, loc) = declared.or_else(|| innermost(f.uses.iter().map(|(u, d)| (*u, *d)), offset))?;
    find_def(a, loc)
}

fn offset(src: &SourceFile, line: usize, col: usize) -> Result<usize, Report> {
    src.offset_of(line, col).ok_or_else(|| usage(format!("{} has no line {line}, column {col}", src.name)))
}

pub fn type_at_in(files: Files, file: &str, line: usize, col: usize) -> Report {
    let (sources, a, skipped) = try_report!(analyze(files, file));
    let src = &sources[0];
    let at = try_report!(offset(src, line, col));
    let expr = innermost(a.facts.exprs.iter().map(|(s, t)| (*s, t.clone())), at);
    let def = named_at(&a, at);
    if expr.is_none() && def.is_none() {
        return failed(json!({ "ok": false, "error": format!("no expression or name at {line}:{col}") }), {
            format!("error: no expression or name at {line}:{col}\n")
        });
    }
    let (errors, note) = errors_note(&a, skipped);
    let mut v = json!({ "ok": true, "line": line, "col": col, "errors_in_file": errors });
    let mut human = String::new();
    if let Some((span, ty)) = &expr {
        v["text"] = json!(src.slice(*span));
        v["type"] = json!(ty);
        v["range"] = place(src, *span);
        human.push_str(&format!("{}: {ty}\n", src.slice(*span)));
    }
    if let Some(d) = def {
        v["declaration"] = def_json(&sources, d);
        human.push_str(&format!("{} {} declared at {}: {}\n", d.kind, d.name, sources[d.file].site(d.span), d.detail));
    }
    human.push_str(&note);
    Report::ok(v, human)
}

/// `LINE:COL`, as `refs` takes a position.
fn position(target: &str) -> Option<(usize, usize)> {
    let (l, c) = target.split_once(':')?;
    Some((l.parse().ok()?, c.parse().ok()?))
}

/// Where a declaration is used in the root file.
fn uses_of(a: &Analysis, d: &Def) -> Vec<Span> {
    let mut uses: Vec<Span> = a.facts.uses.iter().filter(|(_, l)| *l == d.loc()).map(|(u, _)| *u).collect();
    uses.sort_by_key(|s| s.start);
    uses.dedup();
    uses
}

/// The declarations `refs` lists for a name: the root's own, and imported ones it uses.
fn defs_named<'a>(a: &'a Analysis, name: &str) -> Vec<&'a Def> {
    let item = |d: &&Def| !matches!(d.kind, "param" | "local");
    let used = |d: &&Def| d.file == 0 || a.facts.uses.iter().any(|(_, l)| *l == d.loc());
    a.facts.defs.iter().filter(item).filter(used).filter(|d| d.name == name).collect()
}

pub fn refs_in(files: Files, file: &str, target: &str) -> Report {
    let (sources, a, skipped) = try_report!(analyze(files, file));
    let src = &sources[0];
    let defs: Vec<&Def> = match position(target) {
        Some((line, col)) => {
            let at = try_report!(offset(src, line, col));
            match named_at(&a, at) {
                Some(d) => vec![d],
                None => {
                    let m = format!("no declared name at {line}:{col}");
                    return failed(json!({ "ok": false, "error": m }), format!("error: {m}\n"));
                }
            }
        }
        None => defs_named(&a, target),
    };
    if defs.is_empty() {
        let mut names: Vec<&str> = a
            .facts
            .defs
            .iter()
            .filter(|d| d.file == 0 && !matches!(d.kind, "param" | "local"))
            .map(|d| d.name.as_str())
            .collect();
        names.sort();
        names.dedup();
        let owned: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let suggestion = anvil_front::check::closest(target, owned.iter());
        let mut m = format!("nothing called `{target}` is declared in {file} or imported by it");
        if let Some(s) = &suggestion {
            m.push_str(&format!("; did you mean `{s}`?"));
        }
        let json = json!({ "ok": false, "error": m, "suggestion": suggestion, "declared": names });
        return failed(json, format!("error: {m}\ndeclared: {}\n", names.join(", ")));
    }
    let mut out = Vec::new();
    let mut human = String::new();
    for d in defs {
        human.push_str(&format!("{} {} declared at {}: {}\n", d.kind, d.name, sources[d.file].site(d.span), d.detail));
        let list: Vec<_> = uses_of(&a, d)
            .iter()
            .map(|u| {
                let mut v = place(src, *u);
                let line = src.line_col(u.start).0;
                v["source"] = json!(src.line_text(line).trim());
                human.push_str(&format!("  {}  {}\n", src.site(*u), src.line_text(line).trim()));
                v
            })
            .collect();
        human.push_str(&format!("  {} use(s) in {}\n", list.len(), src.name));
        let mut v = def_json(&sources, d);
        v["uses"] = json!(list);
        out.push(v);
    }
    let (errors, note) = errors_note(&a, skipped);
    human.push_str(&note);
    Report::ok(json!({ "ok": true, "declarations": out, "errors_in_file": errors }), human)
}

/// Stands in for the name being typed when there is none yet, so `p.` parses as `p.x`.
const PLACEHOLDER: &str = "__anvil_completes_here";

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// One candidate: its kind decides its group, the order completion lists groups in.
struct Item {
    label: String,
    kind: &'static str,
    detail: String,
}

const GROUPS: &[&str] =
    &["local", "param", "field", "method", "variant", "fn", "struct", "enum", "type", "unit", "builtin", "keyword"];

fn group(kind: &str) -> usize {
    GROUPS.iter().position(|g| *g == kind).expect("every kind has a group")
}

/// What may follow the cursor: in member position (`p.ma|`), a field or method of the
/// receiver's type or a variant of the enum named; elsewhere every name in scope there, the
/// built-ins, the types and the keywords. Only candidates starting with the text already typed.
pub fn complete_in(files: Files, file: &str, line: usize, col: usize) -> Report {
    let src = try_report!(files.load(file));
    let at = try_report!(offset(&src, line, col));
    let start = src.text[..at].char_indices().rev().take_while(|(_, c)| ident_char(*c)).last().map_or(at, |(i, _)| i);
    let prefix = &src.text[start..at];
    let dot = src.text[..start].strip_suffix('.').map(|t| t.len());
    // The text as it is, or with a placeholder name at the cursor (and a `;`) so the item
    // being written parses; other items that do not parse are left out.
    let mut first_failure = None;
    let mut found = None;
    for extra in ["", PLACEHOLDER, &format!("{PLACEHOLDER};")] {
        let text = format!("{}{extra}{}", &src.text[..at], &src.text[at..]);
        let probe = SourceFile::new(src.name.clone(), text);
        match learn(files, &probe, Some(at)) {
            Ok(learned) => {
                found = Some(learned);
                break;
            }
            Err(r) => {
                first_failure.get_or_insert(r);
            }
        }
    }
    let Some((_, a, skipped)) = found else {
        let mut r = first_failure.expect("at least one attempt");
        r.stderr.push_str(
            "completion needs the item being written to parse up to the cursor; fix the syntax error first\n",
        );
        return r;
    };
    let mut items: Vec<Item> = Vec::new();
    let mut v = json!({ "ok": true, "line": line, "col": col, "prefix": prefix, "errors_in_file": 0 });
    match dot {
        Some(dot) => {
            if let Some(e) = enum_before(&a, dot) {
                v["context"] = json!("variant");
                v["receiver"] = json!(e.name);
                items.extend(members(&a, &e.name, "variant"));
            } else {
                // The receiver: the longest expression ending at the dot.
                let receiver =
                    a.facts.exprs.iter().filter(|(s, _)| s.end == dot && s.start < dot).min_by_key(|(s, _)| s.start);
                let Some((span, ty)) = receiver else {
                    let (l, c) = src.line_col(dot);
                    let m = format!("nothing before the `.` at {l}:{c} has a known type");
                    return failed(json!({ "ok": false, "error": m }), format!("error: {m}\n"));
                };
                v["context"] = json!("member");
                v["receiver"] = json!(src.slice(*span));
                v["receiver_type"] = json!(ty);
                let base = ty.split('[').next().unwrap_or(ty);
                items.extend(members(&a, base, "field"));
                items.extend(methods(ty));
            }
        }
        None => {
            v["context"] = json!("name");
            items.extend(in_scope(&a, at));
        }
    }
    let errors = a.diags.iter().filter(|d| !d.message.contains(PLACEHOLDER)).count() + skipped;
    v["errors_in_file"] = json!(errors);
    items.retain(|i| i.label.starts_with(prefix) && i.label != PLACEHOLDER);
    items.sort_by(|x, y| (group(x.kind), &x.label).cmp(&(group(y.kind), &y.label)));
    items.dedup_by(|x, y| x.label == y.label && x.kind == y.kind);
    let mut human = String::new();
    let width = items.iter().map(|i| i.label.len()).max().unwrap_or(0);
    for i in &items {
        human.push_str(&format!("{:width$}  {:8}  {}\n", i.label, i.kind, i.detail));
    }
    v["items"] = json!(items
        .iter()
        .map(|i| json!({ "label": i.label, "kind": i.kind, "detail": i.detail }))
        .collect::<Vec<_>>());
    Report::ok(v, human)
}

/// The enum named just before the dot at `dot` (`Shape.` or `Tree.`), unless a local has that name.
fn enum_before(a: &Analysis, dot: usize) -> Option<&Def> {
    let (_, loc) = a.facts.uses.iter().find(|(u, _)| u.end == dot)?;
    let d = find_def(a, *loc)?;
    let variants = a.facts.defs.iter().any(|v| v.kind == "variant" && v.name.starts_with(&format!("{}.", d.name)));
    (d.kind == "enum" || (d.kind == "type" && variants)).then_some(d)
}

/// The fields of struct `base` or the variants of enum `base`: declarations `base.x` of `kind`.
fn members(a: &Analysis, base: &str, kind: &'static str) -> Vec<Item> {
    let dotted = format!("{base}.");
    a.facts
        .defs
        .iter()
        .filter(|d| d.kind == kind)
        .filter_map(|d| d.name.strip_prefix(&dotted).filter(|m| !m.contains('.')).map(|m| (m, d)))
        .map(|(m, d)| Item { label: m.to_string(), kind, detail: d.detail.clone() })
        .collect()
}

/// The built-in methods of a type shown as `ty`, with its element, key and value types filled in.
fn methods(ty: &str) -> Vec<Item> {
    let (kind, subst): (&str, Vec<(&str, String)>) = if ty == "str" {
        ("str", Vec::new())
    } else if let Some(elem) = ty.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        ("array", vec![("T", elem.to_string())])
    } else if let Some(kv) = ty.strip_prefix("Map[").and_then(|t| t.strip_suffix(']')) {
        let (k, v) = split_top(kv);
        ("map", vec![("K", k), ("V", v)])
    } else {
        return Vec::new();
    };
    METHODS
        .iter()
        .filter(|(on, _, _)| *on == kind)
        .map(|(_, name, sig)| Item { label: name.to_string(), kind: "method", detail: fill(sig, &subst) })
        .collect()
}

/// `K, V` of a map type: split at the comma outside brackets.
fn split_top(kv: &str) -> (String, String) {
    let mut depth = 0;
    for (i, c) in kv.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => return (kv[..i].trim().to_string(), kv[i + 1..].trim().to_string()),
            _ => {}
        }
    }
    (kv.to_string(), String::new())
}

/// Replaces each whole-word type letter in a signature.
fn fill(sig: &str, subst: &[(&str, String)]) -> String {
    let mut out = String::new();
    let mut word = String::new();
    for c in sig.chars().chain(std::iter::once('\0')) {
        if ident_char(c) {
            word.push(c);
            continue;
        }
        match subst.iter().find(|(k, _)| *k == word) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&word),
        }
        word.clear();
        if c != '\0' {
            out.push(c);
        }
    }
    out
}

/// Every name usable at `at` in the root: its locals and parameters in scope there, its own
/// items, the items it imports, the built-ins, the type names and the keywords.
fn in_scope(a: &Analysis, at: usize) -> Vec<Item> {
    let mut items = Vec::new();
    let imported = |d: &Def| a.facts.uses.iter().any(|(_, l)| *l == d.loc());
    for d in &a.facts.defs {
        let visible = match d.kind {
            "param" | "local" => {
                d.file == 0 && d.span.end <= at && d.scope.is_some_and(|s| s.start <= at && at <= s.end)
            }
            "fn" | "struct" | "enum" | "type" | "unit" => d.file == 0 || imported(d),
            _ => false,
        };
        if visible {
            items.push(Item { label: d.name.clone(), kind: d.kind, detail: d.detail.clone() });
        }
    }
    for (name, sig) in BUILTIN_SIGNATURES {
        debug_assert!(BUILTINS.contains(name) || CONSTRUCTORS.contains(name));
        items.push(Item { label: name.to_string(), kind: "builtin", detail: sig.to_string() });
    }
    for t in TYPE_NAMES {
        items.push(Item { label: t.to_string(), kind: "type", detail: "built-in type".into() });
    }
    for k in KEYWORDS {
        items.push(Item { label: k.to_string(), kind: "keyword", detail: String::new() });
    }
    items
}
