//! Diagnostics: every error has a stable code, an exact location and, where one exists, typed fixes.
//! Fix kinds matter: `Mechanical` fixes are safe to apply without thought; `ChangesMeaning` fixes
//! compile but may hide the real bug, so an agent must reason before applying them.

use crate::span::{SourceFile, Span};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    Mechanical,
    ChangesMeaning,
}

#[derive(Clone, Debug)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Fix {
    pub kind: FixKind,
    pub description: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
    pub notes: Vec<(String, Option<Span>)>,
    pub fixes: Vec<Fix>,
    /// Which file of the program the spans are in: 0 is the root, then each imported module.
    pub file: usize,
}

impl Diagnostic {
    pub fn new(code: &'static str, span: Span, message: impl Into<String>) -> Diagnostic {
        assert!(explain(code).is_some(), "diagnostic code {code} has no explanation");
        Diagnostic { code, message: message.into(), span, notes: Vec::new(), fixes: Vec::new(), file: 0 }
    }
    pub fn note(mut self, text: impl Into<String>, span: Option<Span>) -> Diagnostic {
        self.notes.push((text.into(), span));
        self
    }
    pub fn fix(mut self, kind: FixKind, description: impl Into<String>, edits: Vec<Edit>) -> Diagnostic {
        self.fixes.push(Fix { kind, description: description.into(), edits });
        self
    }
    pub fn fix1(
        self,
        kind: FixKind,
        description: impl Into<String>,
        span: Span,
        text: impl Into<String>,
    ) -> Diagnostic {
        self.fix(kind, description, vec![Edit { span, text: text.into() }])
    }
}

#[derive(Serialize)]
struct JsonPos {
    line: usize,
    col: usize,
    offset: usize,
}

#[derive(Serialize)]
struct JsonRange {
    start: JsonPos,
    end: JsonPos,
}

#[derive(Serialize)]
struct JsonEdit {
    range: JsonRange,
    text: String,
}

#[derive(Serialize)]
struct JsonFix {
    kind: FixKind,
    description: String,
    edits: Vec<JsonEdit>,
}

#[derive(Serialize)]
struct JsonNote {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<JsonRange>,
}

#[derive(Serialize)]
pub struct JsonDiagnostic {
    code: &'static str,
    severity: &'static str,
    message: String,
    file: String,
    range: JsonRange,
    source_text: String,
    notes: Vec<JsonNote>,
    fixes: Vec<JsonFix>,
}

fn range(src: &SourceFile, span: Span) -> JsonRange {
    let pos = |o: usize| {
        let (line, col) = src.line_col(o);
        JsonPos { line, col, offset: o }
    };
    JsonRange { start: pos(span.start), end: pos(span.end) }
}

pub fn to_json(src: &SourceFile, d: &Diagnostic) -> JsonDiagnostic {
    JsonDiagnostic {
        code: d.code,
        severity: "error",
        message: d.message.clone(),
        file: src.name.clone(),
        range: range(src, d.span),
        source_text: src.slice(d.span).to_string(),
        notes: d.notes.iter().map(|(m, s)| JsonNote { message: m.clone(), range: s.map(|s| range(src, s)) }).collect(),
        fixes: d
            .fixes
            .iter()
            .map(|f| JsonFix {
                kind: f.kind,
                description: f.description.clone(),
                edits: f.edits.iter().map(|e| JsonEdit { range: range(src, e.span), text: e.text.clone() }).collect(),
            })
            .collect(),
    }
}

/// Human-readable rendering with a source excerpt and caret underline.
pub fn render(src: &SourceFile, d: &Diagnostic) -> String {
    let mut out = String::new();
    let (line, col) = src.line_col(d.span.start);
    out.push_str(&format!("error[{}]: {}\n", d.code, d.message));
    out.push_str(&format!("  --> {}:{}:{}\n", src.name, line, col));
    let text = src.line_text(line);
    let gutter = line.to_string().len();
    let pad = " ".repeat(gutter);
    out.push_str(&format!("{pad} |\n{line} | {text}\n"));
    let (end_line, end_col) = src.line_col(d.span.end);
    let width = if end_line == line {
        (end_col.saturating_sub(col)).max(1)
    } else {
        text.chars().count().saturating_sub(col - 1).max(1)
    };
    out.push_str(&format!("{pad} | {}{}\n", " ".repeat(col - 1), "^".repeat(width)));
    for (note, span) in &d.notes {
        match span {
            Some(s) => out.push_str(&format!("{pad} = note: {note} ({})\n", src.site(*s))),
            None => out.push_str(&format!("{pad} = note: {note}\n")),
        }
    }
    for f in &d.fixes {
        let kind = match f.kind {
            FixKind::Mechanical => "fix",
            FixKind::ChangesMeaning => "fix (changes meaning, check intent first)",
        };
        out.push_str(&format!("{pad} = {kind}: {}\n", f.description));
    }
    out.push_str(&format!("{pad} = see: anvil explain {}\n", d.code));
    out
}

/// Long explanation for each code: `anvil explain CODE`. Every code the compiler emits must be here.
pub fn explain(code: &str) -> Option<&'static str> {
    Some(match code {
        "E0001" => "Unexpected character. Anvil source is ASCII outside string literals and comments.",
        "E0002" => "Unterminated string literal. Strings end with \" on the same line; escapes are \\n \\t \\\" \\\\.",
        "E0003" => {
            "Invalid number literal. Integers are decimal digits (underscores allowed) and must fit in i64. \
                    Floats need digits on both sides of the point, as in 1.0 or 2.5e-3; there is no 1. or .5."
        }
        "E0004" => {
            "Bad interpolation in a string. \"x = {x}\" inserts the text of x; the part in braces is one \
                    expression on the same line, with no braces inside. Write {{ and }} for literal braces."
        }
        "E0110" => {
            "Not in canonical layout. Anvil has one layout for every program, so a file that \
                    differs from what `anvil fmt` prints is an error. The fix (mechanical) is the formatted \
                    file; `anvil fmt` or `anvil fix` applies it."
        }
        "E0111" => {
            "A comment the formatter cannot place. Comments go on their own line before an item, \
                    statement, field, variant or match arm, or at the end of the line one ends on; a comment \
                    inside an expression or a signature has no place in the canonical layout."
        }
        "E0100" => "Syntax error: the parser expected something else here. The message names what it expected.",
        "E0200" => {
            "Unknown name. Variables, functions, types, units and fields must be declared before use \
                    (functions, structs and units may be declared anywhere in the file)."
        }
        "E0201" => {
            "Duplicate definition. Each function, struct, unit, field, parameter and test name is declared once."
        }
        "E0202" => {
            "Shadowing is not allowed. A name declared in a function cannot be declared again in that \
                    function, even in an inner block. Pick a new, more specific name."
        }
        "E0300" => {
            "Type mismatch. Anvil has no implicit conversions: i64 and f64 never mix, and integer literals \
                    are never floats. Convert explicitly with to_f64(x) or to_i64(x), or write 3.0 for a float. \
                    Text is built with interpolation, never by converting: \"{count} items\"."
        }
        "E0301" => {
            "Unit mismatch. f64 values carry units, written f64[m/s]. + - and comparisons need identical \
                    units; * and / combine them. To drop a unit, divide by a literal: x / 1.0[m]."
        }
        "E0302" => {
            "Operator not defined for these types. Arithmetic is defined for i64 and f64 (% only for i64), \
                    comparisons for i64, f64 and str, == and != also for bool, + also joins two str, && || ! for bool. \
                    Enums are compared with match. There is no operator overloading."
        }
        "E0303" => "Wrong number of arguments.",
        "E0304" => {
            "Argument names. Calls to a function with two or more parameters name every argument, \
                    in parameter order: area(width: 2.0, height: 3.0). Calls with one parameter do not name it. \
                    Named arguments make swapped arguments impossible."
        }
        "E0305" => "Struct literal fields. A struct literal lists every field exactly once, in declaration order.",
        "E0306" => "Unknown field.",
        "E0307" => {
            "Unknown or malformed type. Types are i64, f64 (optionally with a unit, f64[m]), bool, str, \
                    arrays [T], Option[T], Result[T, E] and declared structs and enums."
        }
        "E0308" => {
            "Recursive type. A struct or enum cannot contain itself by value, directly or through other \
                    types; hold the inner values in an array, [T], which stores them separately."
        }
        "E0310" => {
            "Assignment to an immutable binding. Declare it with var instead of let to make it mutable. \
                    Parameters are always immutable: copy one into a var first."
        }
        "E0311" => {
            "Invalid assignment target. Only variables, their fields and array elements (p.x, xs[i].y) \
                    can be assigned."
        }
        "E0320" => "Missing return. A function with a return type must end every path with return.",
        "E0321" => "Return value mismatch: a return value in a function without a return type, or the reverse.",
        "E0322" => "`result` is only valid inside ensures clauses, where it names the return value.",
        "E0323" => "Unreachable code. Nothing may follow return, break or continue in the same block.",
        "E0330" => {
            "Effect not declared. Functions are pure unless they declare effects with uses io. \
                    Calling print, or a function that uses io, requires uses io on the caller."
        }
        "E0331" => "Effect declared but never used. Remove uses io so callers can see the function is pure.",
        "E0332" => "Unknown effect. The only effect in v0.1 is io.",
        "E0340" => "Unused variable or parameter. Remove it, or prefix it with _ if it is unused on purpose.",
        "E0341" => "var never reassigned. Use let; var is reserved for bindings that change.",
        "E0350" => "Invalid main. main takes no parameters and returns nothing or i64 (the exit code, 0..=255).",
        "E0360" => "sqrt of a unit with an odd exponent, which has no unit result (sqrt(f64[m]) is meaningless).",
        "E0370" => "break or continue outside a loop.",
        "E0380" => "Condition is not bool. Anvil has no truthiness: write x != 0, not x.",
        "E0390" => "Contracts and asserts must be pure: they cannot call functions that use io.",
        "E0391" => "Comparison chain. a < b < c is not allowed; write a < b && b < c.",
        "E0392" => {
            "Unused value. An expression statement must be a call; the value of a call that returns \
                    something cannot be dropped silently. Bind it: let _unused: T = f(...);"
        }
        "E0400" => {
            "Match is not exhaustive. A match lists one arm per variant; there is no catch-all arm, so \
                    adding a variant to an enum makes every match that forgets it an error."
        }
        "E0401" => "Duplicate match arm. Each variant appears in one arm.",
        "E0402" => {
            "Match pattern fields. A pattern lists every field of its variant in declaration order: \
                    Rect(width, height) binds both; Rect(width: w, height: _) renames one and ignores the other. \
                    A variant without fields has no parentheses: Empty => { }."
        }
        "E0403" => "match works on enums (including Option and Result). Use if and else for other values.",
        "E0404" => {
            "Type cannot be inferred. None, Ok(x), Err(e) and [] need an expected type from context: a \
                    let with its type, a parameter, a field or a return. Some(x) takes its type from x."
        }
        "E0405" => {
            "Misused `?`. `e?` takes the success value of an Option or Result, or returns the failure \
                    from the function at once, so the function must return the same kind: Option for Option, \
                    Result with the same error type for Result. It may only be applied to the whole value of \
                    let, var, an assignment or return: let n: i64 = parse(text)?;"
        }
        "E0406" => {
            "Unknown method. Arrays have len(), push(x) and pop(); str has len(), contains, find, \
                    starts_with, ends_with and split; maps have len(), has, get, set, remove, keys and values. \
                    push, pop, set and remove change the value in place, so they need a var. Other types \
                    have no methods: write a function."
        }
        "E0407" => {
            "No text form. print and string interpolation take i64, f64, bool and str. Print structs \
                    field by field, arrays in a loop and enums with match."
        }
        "E0408" => "Map key type. Map keys are i64 or str, which have one order every program agrees on, so iteration order is fixed.",
        "E0601" => {
            "Module not found. `import shapes.circle.{area};` reads shapes/circle.anv, a path relative to                     the directory of the file that imports it."
        }
        "E0602" => "The module has no item of that name. Check the spelling, or that the item is declared there.",
        "E0603" => {
            "Private item. Only items declared pub can be imported: write `pub fn`, `pub struct`,                     `pub enum` or `pub unit` in the module. A pub struct shows all its fields, a pub enum its variants."
        }
        "E0604" => "Unused import. Every imported name is used in the file; remove the ones that are not (mechanical).",
        "E0605" => {
            "Import cycle. Modules form a tree: if a imports b, b cannot import a, directly or through                     others. Move what both need into a third module that imports neither."
        }
        "E0606" => {
            "main in an imported module. Only the program's root file defines main; a module that is                     imported is a library. Run a module's tests with `anvil test module.anv`."
        }
        "E0607" => {
            "Private type in a pub item. A pub function, struct or enum cannot use a type its module                     keeps private, since importers could not name it. Make the type pub, or the item private."
        }
        "E0500" => {
            "Unknown lint in an allow. `allow LINT \"reason\"` names one of the lints that \
                    `anvil explain` lists after the error codes."
        }
        "E0501" => {
            "An allow needs a reason. Write why the lint does not apply to this function, so a \
                    reader can judge whether it still holds: allow untested \"prints only; the golden covers it\"."
        }
        "E0502" => "Duplicate allow. Each lint is allowed at most once per function.",
        _ => return None,
    })
}

/// Lints: checks `anvil verify` makes beyond type errors, each switched off for one function by
/// `allow LINT "reason"` before its `fn`. An allow that switches off nothing is a finding too.
pub const LINTS: &[(&str, &str)] = &[(
    "untested",
    "A function that no test and no golden-checked main ever runs. Add a test that calls it, or, \
     if a test cannot reach it, write `allow untested \"why\"` before its `fn`. An allow on a \
     function that does run is reported as stale.",
)];

pub fn lint(name: &str) -> Option<&'static str> {
    LINTS.iter().find(|(n, _)| *n == name).map(|(_, text)| *text)
}

pub const ALL_CODES: &[&str] = &[
    "E0001", "E0002", "E0003", "E0004", "E0100", "E0110", "E0111", "E0200", "E0201", "E0202", "E0300", "E0301",
    "E0302", "E0303", "E0304", "E0305", "E0306", "E0307", "E0308", "E0310", "E0311", "E0320", "E0321", "E0322",
    "E0323", "E0330", "E0331", "E0332", "E0340", "E0341", "E0350", "E0360", "E0370", "E0380", "E0390", "E0391",
    "E0392", "E0400", "E0401", "E0402", "E0403", "E0404", "E0405", "E0406", "E0407", "E0408", "E0500", "E0501",
    "E0502", "E0601", "E0602", "E0603", "E0604", "E0605", "E0606", "E0607",
];
