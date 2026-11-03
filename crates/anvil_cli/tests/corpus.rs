//! End-to-end tests through the real `anvil` binary.
//!
//! The repository is itself an Anvil project (anvil.toml): every program in tests/programs and
//! examples has an approved golden in golden/, and `anvil golden check --native` must pass, so
//! the interpreter and the native executable agree with each other and with the approved output.
//!
//! tests/errors: each file lists `// expect: CODE...`; `// resolve: mechanical` means applying
//! every mechanical fix must give a program that checks cleanly.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn anvil(args: &[&str]) -> Output {
    anvil_in(&root(), args)
}

fn anvil_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anvil")).args(args).current_dir(dir).output().unwrap()
}

fn files(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root().join(dir))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".anv"))
        .map(|n| format!("{dir}/{n}"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no .anv files in {dir}");
    v
}

fn header<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().take_while(|l| l.starts_with("//")).find_map(|l| l.strip_prefix(&format!("// {key}: ")))
}

#[test]
fn every_program_matches_its_golden_on_both_backends() {
    let out = anvil(&["golden", "check", "--native", "--json"]);
    let v = json_of(&out);
    // Modules in tests/programs/lib and the standard library have no main; every other file is a
    // program with a golden.
    let bad: Vec<_> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            let module = ["/lib/", "anvil_front/std/"].iter().any(|d| r["file"].as_str().unwrap().contains(d));
            r["status"] != "ok" && !(r["status"] == "no_main" && module)
        })
        .collect();
    assert!(bad.is_empty() && out.status.success(), "{bad:#?}");
    // Every program of the corpus is in the project and has a main.
    assert_eq!(v["checked"].as_u64().unwrap() as usize, files("tests/programs").len() + files("examples").len());
}

/// A package ships only what is inside its crate, so these files are copied there; the copies
/// must stay the repository's.
#[test]
fn packaged_copies_match_the_repository() {
    let r = root();
    let mut copies = vec![("docs/SPEC.md", "crates/anvil_ops/SPEC.md".to_string())];
    copies.push(("README.md", "crates/anvil_cli/README.md".to_string()));
    for c in ["front", "interp", "codegen", "record", "ops", "cli", "mcp"] {
        for f in ["LICENSE-MIT", "LICENSE-APACHE"] {
            copies.push((f, format!("crates/anvil_{c}/{f}")));
        }
    }
    for (from, copy) in copies {
        let same = std::fs::read(r.join(from)).unwrap() == std::fs::read(r.join(&copy)).unwrap();
        assert!(same, "{copy} differs from {from}: copy {from} over it");
    }
}

#[test]
fn run_reports_panics_exactly() {
    let out = anvil(&["run", "tests/programs/panic_index.anv"]);
    assert_eq!(out.status.code(), Some(101));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "0: 10
1: 20
2: 30
"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "panic: index 3 out of bounds for length 3 at tests/programs/panic_index.anv:5:22
"
    );
    let out = anvil(&["run", "tests/programs/exit_code.anv"]);
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn native_build_is_deterministic() {
    let dir = std::env::temp_dir().join(format!("anvil_det_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join(format!("a{}", std::env::consts::EXE_SUFFIX));
    let b = dir.join(format!("b{}", std::env::consts::EXE_SUFFIX));
    // The second program links a C file, compiled in a fresh directory each time.
    for program in ["examples/projectile.anv", "tests/programs/ffi.anv"] {
        for exe in [&a, &b] {
            let out = anvil(&["build", program, "-o", exe.to_str().unwrap()]);
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        let same = std::fs::read(&a).unwrap() == std::fs::read(&b).unwrap();
        assert!(same, "two builds of {program} differ");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn check_json(path: &str) -> (bool, serde_json::Value) {
    let out = anvil(&["check", path, "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{path}: check --json printed invalid JSON ({e}): {}", String::from_utf8_lossy(&out.stdout))
    });
    (out.status.success(), v)
}

/// Applies every mechanical fix (edits by byte offset, last first).
fn apply_mechanical(text: &str, json: &serde_json::Value) -> String {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for d in json["diagnostics"].as_array().unwrap() {
        for f in d["fixes"].as_array().unwrap() {
            if f["kind"] == "mechanical" {
                for e in f["edits"].as_array().unwrap() {
                    let r = &e["range"];
                    edits.push((
                        r["start"]["offset"].as_u64().unwrap() as usize,
                        r["end"]["offset"].as_u64().unwrap() as usize,
                        e["text"].as_str().unwrap().to_string(),
                    ));
                }
            }
        }
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut s = text.to_string();
    for (start, end, t) in edits {
        s.replace_range(start..end, &t);
    }
    s
}

#[test]
fn errors_have_expected_codes_and_fixes_resolve() {
    let tmp = std::env::temp_dir().join(format!("anvil_fix_{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("lib")).unwrap();
    // The modules the error files import, so fixed copies still find them.
    for e in std::fs::read_dir(root().join("tests/errors/lib")).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), tmp.join("lib").join(e.file_name())).unwrap();
    }
    let mut problems = Vec::new();
    for rel in files("tests/errors") {
        let text = std::fs::read_to_string(root().join(&rel)).unwrap();
        let want: BTreeSet<&str> = header(&text, "expect").expect("missing // expect:").split_whitespace().collect();
        let (ok, json) = check_json(&rel);
        if ok {
            problems.push(format!("{rel}: checked cleanly, expected {want:?}"));
            continue;
        }
        let got: BTreeSet<&str> =
            json["diagnostics"].as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap()).collect();
        if got != want {
            problems.push(format!("{rel}: codes {got:?}, expected {want:?}"));
        }
        for d in json["diagnostics"].as_array().unwrap() {
            let explain = anvil(&["explain", d["code"].as_str().unwrap()]);
            assert!(explain.status.success());
        }
        if header(&text, "resolve") == Some("mechanical") {
            // Round by round, as `anvil fix` does: the layout fix (E0110) comes once the rest is fixed.
            let path = tmp.join(Path::new(&rel).file_name().unwrap());
            let (mut fixed, mut after) = (text.clone(), json.clone());
            let mut resolved = false;
            for _ in 0..3 {
                fixed = apply_mechanical(&fixed, &after);
                std::fs::write(&path, &fixed).unwrap();
                let (ok, next) = check_json(path.to_str().unwrap());
                after = next;
                if ok {
                    resolved = true;
                    break;
                }
            }
            if !resolved {
                problems.push(format!("{rel}: mechanical fixes did not resolve it:\n{fixed}\n{after}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_error_code_has_a_test() {
    let out = anvil(&["explain", "--json"]);
    let all: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Lints come after the codes; verify_reports_untested_functions_and_stale_allows tests them.
    let all: BTreeSet<String> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .filter(|c| c.starts_with('E'))
        .collect();
    let mut tested = BTreeSet::new();
    for rel in files("tests/errors") {
        let text = std::fs::read_to_string(root().join(&rel)).unwrap();
        tested.extend(header(&text, "expect").unwrap().split_whitespace().map(String::from));
    }
    let untested: Vec<_> = all.difference(&tested).collect();
    let unknown: Vec<_> = tested.difference(&all).collect();
    assert!(untested.is_empty(), "error codes without a test in tests/errors: {untested:?}");
    assert!(unknown.is_empty(), "tests expect codes that do not exist: {unknown:?}");
}

#[test]
fn examples_check_and_pass_their_tests() {
    for rel in files("examples") {
        let check = anvil(&["check", &rel]);
        assert!(check.status.success(), "{rel}: {}", String::from_utf8_lossy(&check.stderr));
        // Natively too: the tests compiled into one executable must agree with the interpreter.
        let test = anvil(&["test", &rel, "--native", "--json"]);
        assert!(test.status.success(), "{rel}: {}", String::from_utf8_lossy(&test.stdout));
    }
}

#[test]
fn failing_test_blocks_are_reported() {
    let out = anvil(&["test", "tests/test_blocks.anv", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["passed"], 2);
    assert_eq!(v["failed"], 1);
    let failed = v["tests"].as_array().unwrap().iter().find(|t| t["ok"] == false).unwrap();
    assert_eq!(failed["name"], "deliberately wrong");
    assert_eq!(failed["output"], "about to fail\n");
    assert_eq!(failed["panic"], "assertion failed at tests/test_blocks.anv:16:5: double(2) == 5");
}

#[test]
fn usage_errors_are_loud() {
    assert_eq!(anvil(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(anvil(&["check", "examples/projectile.anv", "--verbose"]).status.code(), Some(2));
    assert_eq!(anvil(&["run", "does/not/exist.anv"]).status.code(), Some(2));
    assert_eq!(anvil(&["explain", "E9999"]).status.code(), Some(2));
    assert_eq!(anvil(&["type-at", "examples/orders.anv", "12"]).status.code(), Some(2));
    assert_eq!(anvil(&["type-at", "examples/orders.anv", "12", "x"]).status.code(), Some(2));
    assert_eq!(anvil(&["type-at", "examples/orders.anv", "999", "1"]).status.code(), Some(2));
    assert_eq!(anvil(&["build", "examples/orders.anv", "--max-steps", "3"]).status.code(), Some(2));
    // `run --json` keeps stdout for the program and reports on stderr.
    let no_main = anvil(&["run", "tests/test_blocks.anv", "--json"]);
    assert_eq!(no_main.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&no_main.stderr).contains("E0350"));
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

/// Column (1-based) of the first `needle` on 1-based `line` of `rel`.
fn col_of(rel: &str, line: usize, needle: &str) -> String {
    let text = std::fs::read_to_string(root().join(rel)).unwrap();
    let l = text.lines().nth(line - 1).unwrap();
    (l.find(needle).unwrap_or_else(|| panic!("`{needle}` not on line {line}: {l}")) + 1).to_string()
}

#[test]
fn type_at_names_types_and_declarations() {
    let f = "examples/orders.anv";
    let field = json_of(&anvil(&["type-at", f, "44", &col_of(f, 44, "weight"), "--json"]));
    assert_eq!(field["type"], "f64[kg]", "{field}");
    assert_eq!(field["declaration"]["name"], "Product.weight", "{field}");
    assert_eq!(field["declaration"]["kind"], "field", "{field}");
    assert_eq!(field["declaration"]["defined_at"]["line"], 7, "{field}");

    let binding = json_of(&anvil(&["type-at", f, "53", &col_of(f, 53, "max_weight"), "--json"]));
    assert_eq!(binding["type"], "f64[kg]", "{binding}");
    assert_eq!(binding["declaration"]["kind"], "local", "{binding}");
    assert_eq!(binding["declaration"]["defined_at"]["line"], 52, "{binding}");

    // Inside an interpolation: the innermost expression is the call, the name is the function.
    let call = json_of(&anvil(&["type-at", f, "84", &col_of(f, 84, "money"), "--json"]));
    assert_eq!(call["text"], "money(value)", "{call}");
    assert_eq!(call["type"], "str", "{call}");
    assert_eq!(call["declaration"]["kind"], "fn", "{call}");
    assert_eq!(call["declaration"]["detail"], "fn money(cents: i64) -> str", "{call}");

    let nothing = anvil(&["type-at", f, "1", "1", "--json"]);
    assert_eq!(nothing.status.code(), Some(1));
}

#[test]
fn type_at_works_in_files_with_type_errors() {
    let v = json_of(&anvil(&["type-at", "tests/errors/E0400_missing_arm.anv", "19", "20", "--json"]));
    assert_eq!(v["declaration"]["name"], "Dir.South", "{v}");
    assert_eq!(v["errors_in_file"], 1, "{v}");
}

#[test]
fn refs_list_every_use() {
    let f = "examples/orders.anv";
    let text = std::fs::read_to_string(root().join(f)).unwrap();
    let money = json_of(&anvil(&["refs", f, "money", "--json"]));
    let uses = money["declarations"][0]["uses"].as_array().unwrap();
    assert_eq!(uses.len(), text.matches("money(").count() - 1, "{money}");
    let weight = json_of(&anvil(&["refs", f, "Product.weight", "--json"]));
    let lines: Vec<i64> =
        weight["declarations"][0]["uses"].as_array().unwrap().iter().map(|u| u["line"].as_i64().unwrap()).collect();
    assert_eq!(lines, vec![44, 92, 93, 94], "{weight}");
    let courier = json_of(&anvil(&["refs", f, "Shipping.Courier", "--json"]));
    let lines: Vec<i64> =
        courier["declarations"][0]["uses"].as_array().unwrap().iter().map(|u| u["line"].as_i64().unwrap()).collect();
    assert_eq!(lines, vec![52, 104, 106], "{courier}");
    let unit = json_of(&anvil(&["refs", f, "kg", "--json"]));
    assert_eq!(unit["declarations"][0]["kind"], "unit", "{unit}");

    let missing = anvil(&["refs", f, "mony", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
    let v = json_of(&missing);
    assert!(v["declared"].as_array().unwrap().iter().any(|n| n == "money"), "{v}");
    assert_eq!(v["suggestion"], "money", "{v}");
}

#[test]
fn queries_follow_imports_into_modules() {
    let f = "tests/programs/modules.anv";
    let call = json_of(&anvil(&["type-at", f, "17", &col_of(f, 17, "area("), "--json"]));
    assert_eq!(call["declaration"]["file"], "tests/programs/lib/geometry.anv", "{call}");
    assert_eq!(call["declaration"]["detail"], "pub fn area(s: Shape) -> f64[m^2]", "{call}");
    // An imported name: declared in the module, used here (its import counts).
    let area = json_of(&anvil(&["refs", f, "area", "--json"]));
    let lines: Vec<i64> =
        area["declarations"][0]["uses"].as_array().unwrap().iter().map(|u| u["line"].as_i64().unwrap()).collect();
    assert_eq!(lines, vec![2, 17], "{area}");
    let variant = json_of(&anvil(&["type-at", f, "15", &col_of(f, 15, "Circle"), "--json"]));
    assert_eq!(variant["declaration"]["name"], "Shape.Circle", "{variant}");
    assert_eq!(variant["declaration"]["file"], "tests/programs/lib/geometry.anv", "{variant}");
    // By position: a loop variable, which a name alone could not pick out.
    let sh = json_of(&anvil(&["refs", f, &format!("16:{}", col_of(f, 16, "sh")), "--json"]));
    assert_eq!(sh["declarations"][0]["kind"], "local", "{sh}");
    assert_eq!(sh["declarations"][0]["uses"].as_array().unwrap().len(), 4, "{sh}");
    // Generic types lead to their declaration too.
    let g = "tests/programs/generics.anv";
    let text = std::fs::read_to_string(root().join(g)).unwrap();
    let line = text.lines().position(|l| l.contains("Pair {") && !l.starts_with("struct")).unwrap() + 1;
    let pair = json_of(&anvil(&["type-at", g, &line.to_string(), &col_of(g, line, "Pair {"), "--json"]));
    assert_eq!(pair["declaration"]["kind"], "type", "{pair}");
}

const SHAPES: &str = "struct Point {
    x: i64,
    y: i64,
}

enum Dir {
    Up,
    Down,
}

fn f(p: Point, names: [str], ages: Map[str, i64]) -> i64 {
    let total: i64 = names.len() + ages.len();
    return p.
}

fn g() -> Dir {
    return Dir.
}

fn h(s: str) -> bool {
    return s.st
}
";

#[test]
fn complete_lists_what_fits_at_a_position() {
    let p = Scratch::new("complete");
    p.write("src/shapes.anv", SHAPES);
    let labels = |v: &serde_json::Value| -> Vec<String> {
        v["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
    };
    // Three items are half-written; each completes, the others left out.
    let fields = p.json(&["complete", "src/shapes.anv", "13", "14"]);
    assert_eq!(fields["context"], "member", "{fields}");
    assert_eq!(fields["receiver_type"], "Point", "{fields}");
    assert_eq!(labels(&fields), ["x", "y"], "{fields}");
    let variants = p.json(&["complete", "src/shapes.anv", "17", "16"]);
    assert_eq!(variants["context"], "variant", "{variants}");
    assert_eq!(labels(&variants), ["Down", "Up"], "{variants}");
    let methods = p.json(&["complete", "src/shapes.anv", "21", "16"]);
    assert_eq!(methods["prefix"], "st", "{methods}");
    assert_eq!(labels(&methods), ["starts_with"], "{methods}");
    // Elsewhere: names in scope first (the local only after its declaration), keywords last.
    p.write("src/names.anv", &SHAPES.replace("return p.", "return total;"));
    let names = p.json(&["complete", "src/names.anv", "13", "5"]);
    let all = labels(&names);
    assert_eq!(&all[..4], &["total", "ages", "names", "p"], "{names}");
    assert!(all.contains(&"Point".to_string()) && all.contains(&"print".to_string()), "{names}");
    assert_eq!(all.last().unwrap(), "while", "{names}");
    // The two broken items left out, and `p` now unused (E0340).
    assert_eq!(names["errors_in_file"], 3, "{names}");
    let before = p.json(&["complete", "src/names.anv", "12", "5"]);
    assert!(!labels(&before).contains(&"total".to_string()), "{before}");
    // A typed prefix narrows; a map's methods have its types filled in.
    p.write("src/maps.anv", "fn f(ages: Map[str, [i64]]) {\n    ages.\n}\n");
    let map = p.json(&["complete", "src/maps.anv", "2", "10"]);
    let get = map["items"].as_array().unwrap().iter().find(|i| i["label"] == "get").unwrap().clone();
    assert_eq!(get["detail"], "get(key: str) -> Option[[i64]]", "{map}");
    // Where the cursor's own item does not parse, completion fails loudly.
    p.write("src/broken.anv", "fn f(p: i64) -> i64 {\n    let x: i64 = (p +\n    return x;\n}\n");
    let broken = p.anvil(&["complete", "src/broken.anv", "3", "5"]);
    assert_eq!(broken.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&broken.stderr).contains("fix the syntax error first"));
}

#[test]
fn lsp_serves_over_stdio() {
    use std::io::{Read, Write};
    let mut child = Command::new(env!("CARGO_BIN_EXE_anvil"))
        .arg("lsp")
        .current_dir(root())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for body in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ] {
        write!(input, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    }
    drop(input);
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert!(child.wait().unwrap().success(), "{out}");
    assert!(out.contains(r#""positionEncoding":"utf-16""#), "{out}");
    assert!(out.contains(r#""id":2,"jsonrpc":"2.0","result":null"#), "{out}");
}

#[test]
fn outline_lists_items_without_checking() {
    let v = json_of(&anvil(&["outline", "examples/orders.anv", "--json"]));
    let items = v["items"].as_array().unwrap();
    let names: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(&names[..4], &["kg", "Product", "Line", "Shipping"], "{v}");
    let f = items.iter().find(|i| i["name"] == "line_total").unwrap();
    assert_eq!(f["signature"], "fn line_total(catalog: [Product], line: Line) -> Result[i64, str]");
    // A file with type errors still has an outline.
    assert!(anvil(&["outline", "tests/errors/E0400_missing_arm.anv"]).status.success());
}

#[test]
fn fix_applies_mechanical_fixes_until_clean() {
    let tmp = std::env::temp_dir().join(format!("anvil_cli_fix_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let clean = tmp.join("clean.anv");
    std::fs::copy(root().join("tests/errors/E0303_print_args.anv"), &clean).unwrap();
    let out = anvil(&["fix", clean.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(anvil(&["check", clean.to_str().unwrap()]).status.success());
    // Errors that need a decision stay, and fix says so.
    let stuck = tmp.join("stuck.anv");
    std::fs::copy(root().join("tests/errors/E0400_missing_arm.anv"), &stuck).unwrap();
    let before = std::fs::read(&stuck).unwrap();
    let out = anvil(&["fix", stuck.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json_of(&out)["diagnostics"][0]["code"], "E0400");
    assert_eq!(std::fs::read(&stuck).unwrap(), before, "a changes_meaning fix was applied");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn run_stops_at_the_step_limit() {
    let out = anvil(&["run", "tests/programs/panic_index.anv", "--max-steps", "5"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("step limit"));
    let out = anvil(&["test", "tests/test_blocks.anv", "--max-steps", "1", "--json"]);
    assert_eq!(json_of(&out)["passed"], 0);
}

/// A scratch project in the temp directory.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("anvil_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        Scratch(dir)
    }
    fn write(&self, rel: &str, text: &str) {
        std::fs::write(self.0.join(rel), text).unwrap();
    }
    fn anvil(&self, args: &[&str]) -> Output {
        anvil_in(&self.0, args)
    }
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = args.to_vec();
        a.push("--json");
        json_of(&self.anvil(&a))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const HELLO: &str = "fn main() uses io {\n    print(\"hello\");\n}\n";

fn hello_project(name: &str) -> Scratch {
    let p = Scratch::new(name);
    p.write("anvil.toml", "[project]\nname = \"hello\"\nfiles = [\"src/*.anv\"]\n");
    p.write("src/hello.anv", HELLO);
    p.write(
        "src/maths.anv",
        "fn twice(n: i64) -> i64 {\n    return n * 2;\n}\n\ntest \"twice\" {\n    assert(twice(2) == 4);\n}\n",
    );
    p.write("docs/ROADMAP.md", "# Roadmap\n\n## M1: Start\n- [x] Hello\n\n## M2: Next\n- [ ] Maths\n- [ ] More\n");
    p
}

#[test]
fn goldens_record_and_review_on_a_project() {
    let p = hello_project("project");
    let list = p.json(&["list"]);
    assert_eq!(list["files"].as_array().unwrap().len(), 2, "{list}");

    // No golden yet: a loud failure that says how to approve one.
    let missing = p.anvil(&["golden", "check", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(json_of(&missing)["results"][0]["status"], "missing");
    let approved = p.json(&["golden", "approve", "src/hello.anv"]);
    assert_eq!(approved["stdout"], "hello\n", "{approved}");
    assert_eq!(approved["change"], "new");
    assert!(p.anvil(&["golden", "check"]).status.success());

    // Clean verify, then a regression opens a finding and fixing it resolves it.
    assert!(p.anvil(&["verify"]).status.success());
    p.write("src/hello.anv", "fn main() uses io {\n    print(\"hullo\");\n}\n");
    let broken = p.anvil(&["verify", "--json"]);
    assert_eq!(broken.status.code(), Some(1));
    let v = json_of(&broken);
    assert_eq!(v["open"][0]["check"], "golden", "{v}");
    assert!(v["open"][0]["message"].as_str().unwrap().contains("printed `hullo`, approved `hello`"), "{v}");
    let id = v["open"][0]["id"].clone();
    p.write("src/hello.anv", HELLO);
    let fixed = p.json(&["verify"]);
    assert_eq!(fixed["findings"]["resolved"], serde_json::json!([id]), "{fixed}");

    // Compile errors become findings with their code and position.
    p.write("src/maths.anv", "fn twice(n: i64) -> i64 {\n    return n * 2.0;\n}\n");
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let open = v["open"].as_array().unwrap();
    assert_eq!(open.len(), 1, "{v}");
    assert_eq!((open[0]["check"].as_str(), open[0]["at"].as_str()), (Some("E0300"), Some("2:12")));
    let findings = p.json(&["record", "query", "findings"]);
    assert_eq!(findings["findings"].as_array().unwrap().len(), 2, "the resolved one stays in the history");
    assert_eq!(p.json(&["record", "query", "--open"])["findings"].as_array().unwrap().len(), 1);
    assert_eq!(p.json(&["record", "query", "runs"])["runs"].as_array().unwrap().len(), 4);

    // Milestones from the roadmap, a decision, and the export.
    let imported = p.json(&["record", "import-roadmap"]);
    assert_eq!(imported["current"]["id"], "M2", "{imported}");
    assert!(p.anvil(&["record", "status", "M2.2", "dropped"]).status.success());
    assert_eq!(p.anvil(&["record", "status", "M9", "done"]).status.code(), Some(2));
    assert_eq!(p.anvil(&["record", "status", "M2.1", "finished"]).status.code(), Some(2));
    assert!(p.anvil(&["record", "decide", "Keep it small", "One file per idea."]).status.success());
    let md = String::from_utf8(p.anvil(&["record", "export"]).stdout).unwrap();
    assert!(md.contains("- [-] M2.2 More"), "{md}");
    assert!(md.contains("Keep it small.** One file per idea."), "{md}");
    assert!(md.contains("[E0300]"), "{md}");

    // The review sheet: written to out/review.md, failing while a finding is open.
    let review = p.anvil(&["review", "--json"]);
    assert_eq!(review.status.code(), Some(1));
    let sheet = std::fs::read_to_string(p.0.join("out/review.md")).unwrap();
    for want in
        ["# hello: review sheet", "current milestone: M2 Next", "`src/maths.anv`", "## Sign-off", "Keep it small"]
    {
        assert!(sheet.contains(want), "review sheet lacks `{want}`:\n{sheet}");
    }
}

#[test]
fn project_files_are_loud() {
    let p = hello_project("loud");
    let out = p.anvil(&["golden", "check", "../elsewhere.anv"]);
    assert_eq!(out.status.code(), Some(2));
    p.write("anvil.toml", "[project]\nname = \"x\"\nfiles = [\"src/*.anv\"]\nfiels = []\n");
    let out = p.anvil(&["list"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fiels"), "{}", String::from_utf8_lossy(&out.stderr));
    p.write("anvil.toml", "[project]\nname = \"x\"\nfiles = [\"lib/*.anv\"]\n");
    assert!(String::from_utf8_lossy(&p.anvil(&["list"]).stderr).contains("matches no file"));
    std::fs::remove_file(p.0.join("anvil.toml")).unwrap();
    let out = p.anvil(&["verify"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("[project]"), "the error shows a starting anvil.toml");
}

#[test]
fn trace_shows_calls_returns_and_prints_in_order() {
    let v = json_of(&anvil(&["trace", "examples/projectile.anv", "--limit", "6", "--json"]));
    let e = &v["events"];
    assert_eq!((e[0]["call"].as_str(), e[0]["depth"].as_u64()), (Some("main"), Some(1)), "{v}");
    assert_eq!(e[1]["call"], "flight_steps");
    assert_eq!(e[1]["args"], "start: Body { mass: 0.43[kg], height: 0.0[m], speed: 20.0[m/s] }, dt: 0.01[s]");
    assert_eq!((e[3]["return"].as_str(), e[3]["value"].as_str()), (Some("weight"), Some("-4.2183[kg*m/s^2]")));
    assert_eq!(e.as_array().unwrap().len(), 6);
    assert_eq!((v["limit"].as_u64(), v["dropped"].as_u64()), (Some(6), Some(1630)), "{v}");
    assert_eq!(v["end"], serde_json::json!({ "exit": 0 }));
    // Steps, not time: the same trace every run.
    assert_eq!(json_of(&anvil(&["trace", "examples/projectile.anv", "--limit", "6", "--json"])), v);

    // `only` keeps those functions' calls; prints are always kept. A test can be traced too.
    let v = json_of(&anvil(&["trace", "examples/orders.anv", "--only", "money", "--json"]));
    let events = v["events"].as_array().unwrap();
    assert!(events.iter().all(|e| e.get("call").is_none_or(|c| c == "money")), "{v}");
    assert!(events.iter().any(|e| e.get("call").is_some()) && events.iter().any(|e| e.get("print").is_some()), "{v}");
    let t = json_of(&anvil(&["trace", "examples/orders.anv", "--test", "pickup is free", "--json"]));
    assert_eq!(t["run"], "test \"pickup is free\"");
    assert_eq!(t["end"], serde_json::json!({ "passed": true }));

    // A panic ends the timeline; it is what was observed, not a failure of trace.
    let out = anvil(&["trace", "tests/programs/panic_index.anv"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("       5    | 0: 10\n"), "{text}");
    assert!(text.ends_with("end: panic: index 3 out of bounds for length 3 at tests/programs/panic_index.anv:5:22\n"));

    // Loud about what it cannot do.
    for (args, want) in [
        (
            vec!["trace", "examples/orders.anv", "--test", "nope"],
            "no test \"nope\"; its tests: \"money keeps two digits\"",
        ),
        (vec!["trace", "examples/orders.anv", "--only", "fnd"], "did you mean `find`?"),
        (vec!["trace", "examples/orders.anv", "--limit", "0"], "at least 1"),
        (vec!["profile", "tests/programs/arith.anv", "--test", "x"], "its tests: it has none"),
    ] {
        let out = anvil(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{args:?}: {err}");
    }
}

#[test]
fn trace_writes_values_as_anvil_source_and_cuts_long_ones() {
    let p = Scratch::new("values");
    p.write(
        "v.anv",
        r#"enum Shape {
    Circle(radius: f64),
    Rect(width: f64, height: f64),
    Empty,
}

struct Box {
    label: str,
    sizes: [i64],
    shape: Shape,
}

fn keep(b: Box, maybe: Option[i64], shapes: [Shape]) -> Result[i64, str] {
    match maybe {
        Some(value) => {
            if value == shapes.len() {
                return Ok(value);
            }
        }
        None => {}
    }
    return Err(b.label);
}

fn main() {
    var sizes: [i64] = [];
    for i in 0..20 {
        sizes.push(i);
    }
    let b: Box = Box { label: "a \"quoted\"\nlabel", sizes: sizes, shape: Shape.Circle(1.5) };
    let shapes: [Shape] = [Shape.Rect(width: 1.0, height: 2.0), Shape.Empty];
    let r: Result[i64, str] = keep(b: b, maybe: Some(3), shapes: shapes);
    match r {
        Ok(value: _) => {}
        Err(error: _) => {}
    }
}
"#,
    );
    let v = p.json(&["trace", "v.anv", "--only", "keep"]);
    assert_eq!(
        v["events"][0]["args"],
        r#"b: Box { label: "a \"quoted\"\nlabel", sizes: [0, 1, 2, 3, 4, 5, 6, 7, ... 12 more], shape: Shape.Circle(1.5) }, maybe: Some(3), shapes: [Shape.Rect(width: 1.0, height: 2.0), Shape.Empty]"#,
        "{v}"
    );
    assert_eq!(v["events"][1]["value"], r#"Err("a \"quoted\"\nlabel")"#);

    p.write(
        "long.anv",
        r#"fn echo(s: str) -> str {
    return s;
}

fn main() -> i64 {
    var s: str = "";
    for _i in 0..300 {
        s = s + "x";
    }
    let t: str = echo(s);
    return t.len() - 300;
}
"#,
    );
    let v = p.json(&["trace", "long.anv", "--only", "echo"]);
    assert_eq!(v["events"][0]["args"], format!("s: \"{}\" ... 220 more bytes", "x".repeat(80)), "{v}");

    p.write(
        "map.anv",
        r#"fn total(m: Map[str, i64]) -> i64 {
    var sum: i64 = 0;
    for v in m.values() {
        sum = sum + v;
    }
    return sum;
}

fn main() -> i64 {
    let empty: Map[str, i64] = [:];
    return total(["b": 2, "a": 1]) - 3 + total(empty);
}
"#,
    );
    let v = p.json(&["trace", "map.anv", "--only", "total"]);
    assert_eq!(v["events"][0]["args"], r#"m: ["a": 1, "b": 2]"#, "{v}");
    assert_eq!(v["events"][2]["args"], "m: [:]", "{v}");
}

#[test]
fn profile_counts_calls_and_steps_per_function() {
    let v = json_of(&anvil(&["profile", "tests/programs/recursion.anv", "--json"]));
    let f = |name: &str| v["functions"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap().clone();
    assert_eq!((f("fib")["calls"].as_u64(), f("fib")["steps"].as_u64()), (Some(21891), Some(43782)), "{v}");
    // A recursive function's own nested calls count once in its total; main's total is the run.
    assert_eq!(f("main")["steps"], v["steps"]);
    assert_eq!(v["steps"], 53842);
    let own: u64 = v["functions"].as_array().unwrap().iter().map(|f| f["self_steps"].as_u64().unwrap()).sum();
    assert_eq!(own, 53842, "own steps add up to the whole run");
    assert_eq!(v["functions"][0]["name"], "fib", "most expensive first");
    let t = json_of(&anvil(&["profile", "examples/orders.anv", "--test", "money keeps two digits", "--json"]));
    assert_eq!(t["functions"].as_array().unwrap().len(), 1, "{t}");
    assert!(t["not_run"].as_array().unwrap().contains(&serde_json::json!("find")));
}

#[test]
fn verify_reports_untested_functions_and_stale_allows() {
    let p = hello_project("untested");
    p.write(
        "src/maths.anv",
        r#"fn twice(n: i64) -> i64 {
    return n * 2;
}

fn thrice(n: i64) -> i64 {
    return n * 3;
}

allow untested "kept for the next milestone"
fn half(n: i64) -> i64 {
    return n / 2;
}

test "twice" {
    assert(twice(2) == 4);
}
"#,
    );
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let open: Vec<(&str, &str, &str)> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["check"].as_str().unwrap(), f["file"].as_str().unwrap(), f["at"].as_str().unwrap()))
        .collect();
    // main is left to its golden (missing here); `half` is allowed; `thrice` never runs.
    assert_eq!(open, [("golden", "src/hello.anv", ""), ("untested", "src/maths.anv", "5:4")], "{v}");

    p.write(
        "src/maths.anv",
        "allow untested \"stale\"\nfn twice(n: i64) -> i64 {\n    return n * 2;\n}\n\ntest \"twice\" {\n    assert(twice(2) == 4);\n}\n",
    );
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let stale = v["open"].as_array().unwrap().iter().find(|f| f["check"] == "allow").unwrap();
    assert!(stale["message"].as_str().unwrap().contains("`twice` runs"), "{v}");
    let outline = p.json(&["outline", "src/maths.anv"]);
    assert_eq!(outline["items"][0]["allows"], serde_json::json!([{ "lint": "untested", "reason": "stale" }]));
    assert!(String::from_utf8(anvil(&["explain", "untested"]).stdout).unwrap().starts_with("untested: "));
}

#[test]
fn verify_reports_unreached_branches_and_stale_allows() {
    let p = hello_project("unreached");
    p.write(
        "src/maths.anv",
        r#"enum Shape {
    Dot,
    Line(n: i64),
}

fn classify(n: i64) -> str {
    if n < 0 {
        return "negative";
    } else {
        return "other";
    }
}

fn size(s: Shape) -> i64 {
    var out: i64 = 0;
    match s {
        Dot => {}
        Line(n) => { out = n; }
    }
    return out;
}

fn total(xs: [i64]) -> i64 {
    var t: i64 = 0;
    for x in xs {
        t = t + x;
    }
    return t;
}

fn pick[T](xs: [T], first: bool) -> T {
    if first {
        return xs[0];
    }
    return xs[xs.len() - 1];
}

fn apply(n: i64) -> i64 {
    let f: fn(i64) -> i64 = fn(v: i64) -> i64 {
        if v > 100 {
            return 0;
        }
        return v;
    };
    return f(n);
}

allow unreached "kept for later"
fn sign(n: i64) -> i64 {
    if n < 0 {
        return -1;
    }
    return 1;
}

test "some branches" {
    assert(classify(5) == "other");
    assert(size(Shape.Dot) == 0);
    assert(total([]) == 0);
    assert(pick(xs: [1, 2], first: false) == 2);
    assert(apply(3) == 3);
    assert(sign(-2) == -1);
}
"#,
    );
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let mut open: Vec<(&str, &str)> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["file"] == "src/maths.anv")
        .map(|f| (f["check"].as_str().unwrap(), f["at"].as_str().unwrap()))
        .collect();
    open.sort();
    // The `if` never true, the arm never matched, the loop never entered, the branch of a
    // generic and of a function literal; the empty `Dot` arm is no branch; `sign` is allowed,
    // but all of its branches run.
    assert_eq!(
        open,
        [
            ("allow", "49:4"),
            ("unreached", "18:20"),
            ("unreached", "25:17"),
            ("unreached", "32:14"),
            ("unreached", "40:20"),
            ("unreached", "7:14")
        ],
        "{v}"
    );
    let file = v["files"].as_array().unwrap().iter().find(|f| f["file"] == "src/maths.anv").unwrap();
    assert_eq!(file["branches"], serde_json::json!({ "total": 7, "taken": 2 }), "{file}");
    assert_eq!(file["unreached"][0]["function"], "classify", "{file}");
    let msg = v["open"].as_array().unwrap().iter().find(|f| f["at"] == "40:20").unwrap()["message"].clone();
    assert!(msg.as_str().unwrap().contains("in `apply`"), "a literal's branch belongs to its function: {msg}");
    assert!(String::from_utf8(anvil(&["explain", "unreached"]).stdout).unwrap().starts_with("unreached: "));
}

#[test]
fn a_differing_golden_leaves_the_actual_output_and_a_diff() {
    let p = hello_project("golden_diff");
    let program = |second: &str| {
        format!("fn main() uses io {{\n    print(\"one\");\n    print(\"{second}\");\n    print(\"three\");\n}}\n")
    };
    p.write("src/hello.anv", &program("two"));
    assert!(p.anvil(&["golden", "approve", "src/hello.anv"]).status.success());
    p.write("src/hello.anv", &program("2"));
    let v = json_of(&p.anvil(&["golden", "check", "--json"]));
    let detail = v["results"][0]["detail"].as_str().unwrap();
    assert!(
        detail.ends_with("actual output in out/golden/src/hello.anv.out, diff in out/golden/src/hello.anv.diff"),
        "{v}"
    );
    let read = |rel: &str| std::fs::read_to_string(p.0.join(rel)).unwrap();
    assert_eq!(read("out/golden/src/hello.anv.out"), "exit: 0\n---\none\n2\nthree\n");
    assert_eq!(read("out/golden/src/hello.anv.diff"), "...\n  ---\n  one\n- two\n+ 2\n  three\n");
    // Matching again removes them, so out/golden never shows a stale failure.
    p.write("src/hello.anv", &program("two"));
    assert!(p.anvil(&["golden", "check"]).status.success());
    assert!(!p.0.join("out/golden/src/hello.anv.out").exists());
}

#[test]
fn watch_verifies_again_after_a_change() {
    use std::io::{BufRead, BufReader};
    let p = hello_project("watch");
    assert!(p.anvil(&["golden", "approve", "src/hello.anv"]).status.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_anvil"))
        .args(["watch", "--runs", "2", "--json"])
        .current_dir(&p.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next = || -> serde_json::Value { serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap() };
    let first = next();
    assert_eq!(first["ok"], true, "{first}");
    p.write("src/hello.anv", "fn main() uses io {\n    print(\"hullo there\");\n}\n");
    let second = next();
    assert_eq!(
        (second["ok"].as_bool(), second["open"][0]["check"].as_str()),
        (Some(false), Some("golden")),
        "{second}"
    );
    assert_eq!(next(), serde_json::json!({ "ok": false, "runs": 2 }));
    assert_eq!(child.wait().unwrap().code(), Some(1));
}

#[test]
fn fmt_and_check_agree_on_the_layout() {
    let p = hello_project("fmt");
    let messy = "fn main() uses io {\n  print(\"hello\");   // greet\n}\n";
    p.write("src/hello.anv", messy);
    // An unformatted file is an error everywhere a file is checked, with the layout as its fix.
    let v = p.json(&["check", "src/hello.anv"]);
    assert_eq!(v["diagnostics"][0]["code"], "E0110", "{v}");
    assert_eq!(v["diagnostics"][0]["fixes"][0]["kind"], "mechanical");
    assert_eq!(p.anvil(&["run", "src/hello.anv"]).status.code(), Some(1));

    let checked = p.anvil(&["fmt", "--check", "--json"]);
    assert_eq!(checked.status.code(), Some(1));
    let v = json_of(&checked);
    assert_eq!(
        v["files"],
        serde_json::json!([{ "file": "src/hello.anv", "changed": true }, { "file": "src/maths.anv", "changed": false }])
    );
    assert_eq!(std::fs::read_to_string(p.0.join("src/hello.anv")).unwrap(), messy, "--check writes nothing");

    assert!(p.anvil(&["fmt"]).status.success());
    let formatted = "fn main() uses io {\n    print(\"hello\"); // greet\n}\n";
    assert_eq!(std::fs::read_to_string(p.0.join("src/hello.anv")).unwrap(), formatted);
    assert!(p.anvil(&["check", "src/hello.anv"]).status.success());
    assert!(p.anvil(&["fmt", "--check"]).status.success());

    // `fix` applies the layout too, once the other errors are fixed.
    p.write("src/hello.anv", "fn main() -> i64 {\n  var x: i64 = 1;\n  return x;\n}\n");
    let v = p.json(&["fix", "src/hello.anv"]);
    assert_eq!(v["ok"], true, "{v}");
    let codes: Vec<&str> = v["applied"].as_array().unwrap().iter().map(|a| a["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["E0341", "E0110"], "{v}");
    assert_eq!(
        std::fs::read_to_string(p.0.join("src/hello.anv")).unwrap(),
        "fn main() -> i64 {\n    let x: i64 = 1;\n    return x;\n}\n"
    );

    // A comment with no place in the layout is an error, never dropped.
    p.write("src/hello.anv", "fn main() -> i64 {\n    return 1 + // one\n        2;\n}\n");
    let out = p.anvil(&["fmt", "src/hello.anv"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("E0111"));
}

#[test]
fn syntax_errors_are_reported_one_per_item() {
    let p = Scratch::new("syntax");
    p.write(
        "bad.anv",
        "fn a() -> i64 {\n    let x: i64 = ;\n    return x;\n}\n\nfn b() -> i64 {\n    return 1 +;\n}\n\n\
         struct S { x i64 }\n\nfn main() -> i64 {\n    return 0;\n}\n",
    );
    let v = p.json(&["check", "bad.anv"]);
    let found: Vec<(&str, u64)> = v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["code"].as_str().unwrap(), d["range"]["start"]["line"].as_u64().unwrap()))
        .collect();
    assert_eq!(found, [("E0100", 2), ("E0100", 7), ("E0100", 10)], "{v}");
}

#[test]
fn modules_import_by_name_and_share_coverage() {
    let p = Scratch::new("modules");
    std::fs::create_dir_all(p.0.join("src/text")).unwrap();
    p.write("anvil.toml", "[project]\nname = \"m\"\nfiles = [\"src/*.anv\", \"src/text/*.anv\"]\n");
    p.write(
        "src/text/words.anv",
        "pub fn shout(s: str) -> str {\n    return s + \"!\";\n}\n\npub fn whisper(s: str) -> str {\n    return s;\n}\n\n\
         test \"shout\" {\n    assert(shout(\"a\") == \"a!\");\n}\n",
    );
    p.write("src/main.anv", "import text.words.{shout};\n\nfn main() uses io {\n    print(shout(\"hey\"));\n}\n");
    let run = p.anvil(&["run", "src/main.anv"]);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hey!\n");
    // A module is also a file of its own: its tests run when it is the root.
    assert!(p.anvil(&["test", "src/text/words.anv"]).status.success());
    let outline = p.json(&["outline", "src/main.anv"]);
    assert_eq!(outline["items"][0]["file"], "src/text/words.anv", "{outline}");
    let trace = p.json(&["trace", "src/main.anv", "--only", "text.words.shout"]);
    assert_eq!(trace["events"][0]["call"], "text.words.shout", "{trace}");

    // Coverage counts every project file's runs: shout runs under main's golden, whisper nowhere.
    assert!(p.anvil(&["golden", "approve", "src/main.anv"]).status.success());
    let v = p.json(&["verify"]);
    let open: Vec<(&str, &str)> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["check"].as_str().unwrap(), f["file"].as_str().unwrap()))
        .collect();
    assert_eq!(open, [("untested", "src/text/words.anv")], "{v}");
    assert!(v["open"][0]["message"].as_str().unwrap().contains("`whisper`"), "{v}");

    // An error inside a module names the module's file, and fix leaves other files alone.
    p.write("src/text/words.anv", "pub fn shout(s: str) -> str {\n    return s + 1;\n}\n");
    let v = p.json(&["check", "src/main.anv"]);
    assert_eq!(
        (v["diagnostics"][0]["code"].as_str(), v["diagnostics"][0]["file"].as_str()),
        (Some("E0302"), Some("src/text/words.anv")),
        "{v}"
    );
    let before = std::fs::read_to_string(p.0.join("src/text/words.anv")).unwrap();
    assert_eq!(p.anvil(&["fix", "src/main.anv"]).status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(p.0.join("src/text/words.anv")).unwrap(), before);
}

#[test]
fn generics_are_instantiated_named_and_covered() {
    let p = hello_project("generics");
    p.write(
        "src/hello.anv",
        r#"fn first[T](xs: [T]) -> Option[T] {
    if xs.len() == 0 {
        return None;
    }
    return Some(xs[0]);
}

fn never[T: ordered](a: T, b: T) -> bool {
    return a < b;
}

fn main() uses io {
    let none: [i64] = [];
    print("{first([3, 4]) == Some(3)} {first(["a"]) == Some("a")} {first(none) == None}");
}
"#,
    );
    assert_eq!(String::from_utf8_lossy(&p.anvil(&["run", "src/hello.anv"]).stdout), "true true true\n");
    // One instance per type, named by its types.
    let v = p.json(&["profile", "src/hello.anv"]);
    let mut names: Vec<&str> = v["functions"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["first[i64]", "first[str]", "main"], "{v}");
    let outline = p.json(&["outline", "src/hello.anv"]);
    assert_eq!(outline["items"][1]["signature"], "fn never[T: ordered](a: T, b: T) -> bool");
    // A generic that nothing instantiates is untested; one that runs for any type is tested, and
    // its branches count over all its instances.
    assert!(p.anvil(&["golden", "approve", "src/hello.anv"]).status.success());
    let v = p.json(&["verify"]);
    let open: Vec<&str> = v["open"].as_array().unwrap().iter().map(|f| f["message"].as_str().unwrap()).collect();
    assert_eq!(open.len(), 1, "{v}");
    assert!(open[0].starts_with("`never` never runs"), "{v}");
    // Constraint errors carry the constraint as a fix that changes meaning.
    p.write("src/maths.anv", "fn bigger[T](a: T, b: T) -> bool {\n    return a > b;\n}\n\ntest \"bigger\" {\n    assert(bigger(a: 2, b: 1));\n}\n");
    let v = p.json(&["check", "src/maths.anv"]);
    assert_eq!(v["diagnostics"][0]["code"], "E0703", "{v}");
    assert_eq!(v["diagnostics"][0]["fixes"][0]["kind"], "changes_meaning");
    assert_eq!(v["diagnostics"][0]["fixes"][0]["edits"][0]["text"], ": ordered");
}

#[test]
fn property_tests_find_small_counterexamples_and_native_tests_agree() {
    let p = Scratch::new("properties");
    p.write(
        "p.anv",
        r#"fn largest(xs: [i64]) -> i64 {
    var best: i64 = 0;
    for x in xs {
        if x > best {
            best = x;
        }
    }
    return best;
}

test "the largest is in the array" for all xs: [i64] {
    if xs.len() > 0 {
        var found: bool = false;
        for x in xs {
            if x == largest(xs) {
                found = true;
            }
        }
        assert(found);
    }
}

test "plain and passing" {
    assert(largest([1, 5, 2]) == 5);
}

test "plain and failing" {
    assert(largest([-3]) == -3);
}
"#,
    );
    let v = p.json(&["test", "p.anv"]);
    let t = &v["tests"][0];
    // Seeded and shrunk: the same small counterexample every run.
    assert_eq!((t["ok"].as_bool(), t["inputs"].as_str()), (Some(false), Some("xs: [-1]")), "{v}");
    assert_eq!(p.json(&["test", "p.anv"])["tests"][0], *t, "runs repeat");
    assert_eq!(v["cases"], 100);
    let other_seed = p.json(&["test", "p.anv", "--seed", "7", "--cases", "300"]);
    assert_eq!(other_seed["cases"], 300);
    assert_eq!(other_seed["tests"][0]["ok"], false, "{other_seed}");

    // Natively the plain tests run again and must agree, panics included.
    let v = p.json(&["test", "p.anv", "--native"]);
    assert_eq!(v["tests"][1]["native"], "ok", "{v}");
    let failing = &v["tests"][2];
    assert_eq!(failing["native"].as_str().unwrap(), format!("panic: {}", failing["panic"].as_str().unwrap()));
    assert!(v["tests"][0].get("native").is_none(), "property tests run in the interpreter only");
}

#[test]
fn c_functions_are_traced_and_their_failures_are_loud() {
    let p = Scratch::new("ffi");
    std::fs::create_dir_all(p.0.join("src/c")).unwrap();
    p.write("src/c/twice.c", "#include <stdint.h>\nint64_t twice(int64_t n) { return n * 2; }\n");
    p.write(
        "src/main.anv",
        "extern \"c/twice.c\" fn twice(n: i64) -> i64;\n\nfn main() uses io {\n    print(twice(21));\n}\n",
    );
    let v = p.json(&["trace", "src/main.anv"]);
    let calls: Vec<String> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| match (e["call"].as_str(), e["return"].as_str()) {
            (Some(f), _) => Some(format!("call {f}({})", e["args"].as_str().unwrap())),
            (_, Some(f)) => Some(format!("{f} returned {}", e["value"].as_str().unwrap())),
            _ => None,
        })
        .collect();
    assert_eq!(calls, ["call main()", "call twice(n: 21)", "twice returned 42", "main returned nothing"], "{v}");

    // C that does not compile stops the run, with the compiler's own message.
    p.write("src/c/twice.c", "int64_t twice(int64_t n) { return n * ; }\n");
    for args in [&["run", "src/main.anv"][..], &["build", "src/main.anv"][..]] {
        let out = p.anvil(args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success() && err.contains("could not compile") && err.contains("twice.c"), "{err}");
    }

    // An executable refuses a clock it does not know.
    p.write("src/c/twice.c", "#include <stdint.h>\nint64_t twice(int64_t n) { return n * 2; }\n");
    let exe = p.0.join(format!("main{}", std::env::consts::EXE_SUFFIX));
    assert!(p.anvil(&["build", "src/main.anv", "-o", exe.to_str().unwrap()]).status.success());
    let out = Command::new(&exe).env("ANVIL_CLOCK", "fast").output().unwrap();
    assert_eq!(out.status.code(), Some(70));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ANVIL_CLOCK must be unset"), "{out:?}");
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
}

const ECHO: &str = "fn main() -> i64 uses io, fs {
    for a in args() {
        print(\"arg [{a}]\");
    }
    var n: i64 = 0;
    var more: bool = true;
    while more {
        match read_line() {
            Some(value: line) => {
                print(\"line [{line}]\");
                n = n + 1;
            }
            None => { more = false; }
        }
    }
    match write_file(path: \"out.txt\", text: \"{n} lines\") {
        Ok(value: _) => {}
        Err(error: _) => { return 1; }
    }
    match read_file(\"out.txt\") {
        Ok(value: t) => { print(\"file [{t}]\"); }
        Err(error: _) => { return 2; }
    }
    return 0;
}

test \"t\" {
    assert(true);
}
";

/// Runs `program` in `dir` with `stdin` and `env`, and returns what it did.
fn run_with(program: &Path, args: &[&str], dir: &Path, stdin: &[u8], env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn programs_take_arguments_stdin_and_files() {
    let p = Scratch::new("input");
    p.write("src/echo.anv", ECHO);
    let written = p.0.join("out.txt");
    let take_written = || {
        let text = std::fs::read_to_string(&written).ok();
        let _ = std::fs::remove_file(&written);
        text
    };

    // `anvil run`: the arguments after `--`, its own stdin ("\r\n" ends a line too), real files.
    let anvil_exe = Path::new(env!("CARGO_BIN_EXE_anvil"));
    let out = run_with(anvil_exe, &["run", "src/echo.anv", "--", "h\u{e9}llo", "a b"], &p.0, b"one\r\ntwo", &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text, "arg [h\u{e9}llo]\narg [a b]\nline [one]\nline [two]\nfile [2 lines]\n", "{out:?}");
    assert_eq!(take_written().as_deref(), Some("2 lines"));
    p.write("in.txt", "x\n");
    let out = p.anvil(&["run", "src/echo.anv", "--input", "in.txt"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "line [x]\nfile [1 lines]\n");
    assert_eq!(take_written().as_deref(), Some("1 lines"));

    // Checked runs never write to the disk: a trace keeps its writes in memory.
    // `--json` goes before `--`: everything after it is the program's.
    let v = json_of(&p.anvil(&["trace", "src/echo.anv", "--input", "in.txt", "--json", "--", "z"]));
    let prints: Vec<&str> = v["events"].as_array().unwrap().iter().filter_map(|e| e["print"].as_str()).collect();
    assert_eq!(prints, ["arg [z]", "line [x]", "file [1 lines]"], "{v}");
    assert_eq!(take_written(), None);
    let out = p.anvil(&["trace", "src/echo.anv", "--test", "t", "--", "z"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("tests run without them"), "{out:?}");

    // An executable: real arguments (UTF-16 on Windows, kept as UTF-8), stdin and files.
    let exe = p.0.join(format!("echo{}", std::env::consts::EXE_SUFFIX));
    assert!(p.anvil(&["build", "src/echo.anv", "-o", exe.to_str().unwrap()]).status.success());
    let out = run_with(&exe, &["h\u{e9}llo", ""], &p.0, b"three\n", &[]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "arg [h\u{e9}llo]\narg []\nline [three]\nfile [1 lines]\n");
    assert_eq!(take_written().as_deref(), Some("1 lines"));
    // ANVIL_FS=memory keeps writes off the disk, as in goldens; any other value is loud.
    let out = run_with(&exe, &[], &p.0, b"", &[("ANVIL_FS", "memory")]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "file [0 lines]\n");
    assert_eq!(take_written(), None);
    let out = run_with(&exe, &[], &p.0, b"", &[("ANVIL_FS", "disk")]);
    assert_eq!(out.status.code(), Some(70));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ANVIL_FS must be unset"), "{out:?}");

    // A golden's input file gives it arguments and stdin, on both backends.
    p.write("anvil.toml", "[project]\nname = \"io\"\nfiles = [\"src/*.anv\"]\n");
    std::fs::create_dir_all(p.0.join("golden/src")).unwrap();
    p.write("golden/src/echo.anv.in", "arg: g\n---\nl1\n");
    let v = p.json(&["golden", "approve", "src/echo.anv"]);
    assert_eq!(v["stdout"], "arg [g]\nline [l1]\nfile [1 lines]\n", "{v}");
    let v = p.json(&["golden", "check", "--native"]);
    assert_eq!(v["results"][0]["status"], "ok", "{v}");
    assert_eq!(take_written(), None);
    p.write("golden/src/echo.anv.in", "arg g\n---\n");
    let v = p.json(&["golden", "check"]);
    assert!(v["results"][0]["detail"].as_str().unwrap().contains("unknown line `arg g`"), "{v}");
}

#[test]
fn fuzzed_programs_agree_on_both_backends() {
    let p = Scratch::new("fuzz");
    let v = p.json(&["fuzz", "--seed", "1", "--count", "3"]);
    assert_eq!((v["ok"].as_bool(), v["failed"].as_u64()), (Some(true), Some(0)), "{v}");
    let statuses: Vec<&str> = v["programs"].as_array().unwrap().iter().map(|r| r["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, ["same", "same", "same"]);
}
