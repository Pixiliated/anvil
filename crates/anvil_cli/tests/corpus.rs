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
    assert!(p.anvil(&["usage", "approve"]).status.success());
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
    assert!(p.anvil(&["usage", "approve"]).status.success());
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
    assert!(p.anvil(&["usage", "approve"]).status.success());
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
fn mutate_reports_what_no_test_notices() {
    let p = hello_project("mutate");
    p.write(
        "src/maths.anv",
        r#"pub fn is_adult(age: i64) -> bool {
    return age >= 18;
}

allow mutants "nothing survives here"
fn double(n: i64) -> i64 {
    return n * 2;
}

test "ages and doubles" {
    assert(is_adult(30));
    assert(!is_adult(5));
    assert(double(3) == 6);
}
"#,
    );
    let survivors = |v: &serde_json::Value| -> Vec<(String, String, String)> {
        v["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["status"] == "survived")
            .map(|m| (m["at"].as_str().unwrap().into(), m["from"].as_str().unwrap().into(), m["to"].as_str().unwrap().into()))
            .collect()
    };
    // No test asks about 18 itself, so neither >= to > nor 18 to 19 is noticed; `double`'s allow
    // excuses nothing.
    let out = p.anvil(&["mutate", "src/maths.anv", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v = json_of(&out);
    let want = |a: &str, f: &str, t: &str| (a.to_string(), f.to_string(), t.to_string());
    assert_eq!(survivors(&v), [want("2:16", ">=", ">"), want("2:19", "18", "19")], "{v}");
    assert_eq!(v["stale_allows"][0]["function"], "double", "{v}");
    assert_eq!((v["killed"].as_u64(), v["score"].as_u64()), (Some(2), Some(50)), "{v}");

    // A file that imports it and prints is_adult(18) under a golden notices both.
    p.write("src/app.anv", "import maths.{is_adult};\n\nfn main() uses io {\n    print(\"{is_adult(18)}\");\n}\n");
    assert!(p.anvil(&["golden", "approve", "src/app.anv"]).status.success());
    let v = json_of(&p.anvil(&["mutate", "src/maths.anv", "--json"]));
    assert!(survivors(&v).is_empty(), "{v}");
    assert_eq!(v["against"], serde_json::json!(["src/maths.anv", "src/app.anv"]), "{v}");
    assert_eq!(v["mutants"][0]["by"], "the golden of src/app.anv", "{v}");
    assert_eq!(v["mutants"][2]["by"], "test \"ages and doubles\" in src/maths.anv", "{v}");
    assert_eq!(v["ok"], false, "the stale allow still fails it: {v}");
    let fixed = std::fs::read_to_string(p.0.join("src/maths.anv")).unwrap().replace("allow mutants \"nothing survives here\"\n", "");
    p.write("src/maths.anv", &fixed);
    assert!(p.anvil(&["mutate", "src/maths.anv"]).status.success());

    // An equivalent mutant is excused by an allow with its reason.
    p.write("src/app.anv", "fn main() uses io {\n    print(\"ok\");\n}\n");
    assert!(p.anvil(&["golden", "approve", "src/app.anv"]).status.success());
    p.write("src/maths.anv", &format!("allow mutants \"18 is never asked about\"\n{fixed}"));
    let v = json_of(&p.anvil(&["mutate", "src/maths.anv", "--only", "is_adult", "--json"]));
    assert_eq!((v["ok"].as_bool(), v["allowed"].as_u64(), v["mutants"].as_array().unwrap().len()), (Some(true), Some(2), 2), "{v}");

    // Loud, never silent: an unknown function, or tests that fail before any change.
    assert_eq!(p.anvil(&["mutate", "src/maths.anv", "--only", "nope"]).status.code(), Some(2));
    p.write("src/maths.anv", "fn one() -> i64 {\n    return 1;\n}\n\ntest \"one\" {\n    assert(one() == 2);\n}\n");
    let out = p.anvil(&["mutate", "src/maths.anv"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fails before any change"));
}

#[test]
fn verify_checks_contracts_on_generated_arguments() {
    let p = hello_project("contracts");
    p.write(
        "src/maths.anv",
        r#"fn half_up(n: i64) -> i64
    ensures result * 2 >= n
{
    return n / 2;
}

fn second(xs: [i64]) -> i64
    requires xs.len() > 0
{
    return xs[1];
}

allow contract "kept"
fn first(xs: [i64]) -> i64
    requires xs.len() > 0
{
    return xs[0];
}

fn exact(n: i64) -> i64
    requires n == 12345
{
    return n;
}

fn spin(n: i64) -> i64
    requires n >= 0
{
    var i: i64 = 0;
    while i < n {
        i = i + 1;
    }
    return i;
}

fn shout(s: str) uses io
    requires s.len() > 0
{
    print(s);
}

fn grow(n: i64) -> i64
    ensures result > n
{
    return n + 1;
}
"#,
    );
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let found: Vec<(&str, &str, &str)> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == "contract" || f["check"] == "allow")
        .map(|f| (f["check"].as_str().unwrap(), f["at"].as_str().unwrap(), f["message"].as_str().unwrap()))
        .collect();
    let at: Vec<(&str, &str)> = found.iter().map(|(c, a, _)| (*c, *a)).collect();
    // half_up breaks its ensures, second panics within its requires, first's allow is stale,
    // exact's requires admits nothing generated, spin runs too long; shout has effects and is
    // not checked, and grow only overflows, which counts as outside its contract.
    assert_eq!(at, [("contract", "1:4"), ("contract", "7:4"), ("allow", "14:4"), ("contract", "20:4"), ("contract", "26:4")], "{v}");
    assert!(found[0].2.starts_with("`half_up` breaks its contract for n: 1: ensures failed at src/maths.anv:2:13"), "{v}");
    assert!(found[1].2.contains("for xs: [0]: index 1 out of bounds"), "{v}");
    assert!(found[3].2.contains("checked on none of 100 generated inputs"), "{v}");
    assert!(found[4].2.contains("runs past 1000000 steps for n: 9223372036854775807"), "{v}");
    let file = v["files"].as_array().unwrap().iter().find(|f| f["file"] == "src/maths.anv").unwrap();
    let status = |name: &str| {
        let c = file["contracts"].as_array().unwrap().iter().find(|c| c["function"] == name).unwrap().clone();
        c
    };
    assert_eq!(status("shout")["status"], "unchecked", "{file}");
    assert_eq!(status("grow")["status"], "held", "{file}");
    assert!(status("grow")["overflowed"].as_u64().unwrap() > 0, "{file}");
    assert!(String::from_utf8(anvil(&["explain", "contract"]).stdout).unwrap().starts_with("contract: "));
}

#[test]
fn doc_comments_are_shown_checked_and_required() {
    let p = hello_project("docs");
    p.write(
        "src/maths.anv",
        r#"// Twice n.
// example: twice(4) == 8
pub fn twice(n: i64) -> i64 {
    return n * 2;
}

pub struct Pair {
    a: i64,
    b: i64,
}

// Half of n, rounded toward zero.
// example: half(7) == 4
allow undocumented "kept to see it reported stale"
pub fn half(n: i64) -> i64 {
    return n / 2;
}

test "twice" {
    assert(twice(2) == 4);
}
"#,
    );
    // outline, type-at and complete show the doc with the signature.
    let outline = p.json(&["outline", "src/maths.anv"]);
    assert_eq!(outline["items"][0]["doc"], "Twice n.\nexample: twice(4) == 8", "{outline}");
    assert_eq!(outline["items"][1]["doc"], serde_json::Value::Null, "{outline}");
    let at = p.json(&["type-at", "src/maths.anv", "20", "12"]);
    assert_eq!(at["declaration"]["doc"], "Twice n.\nexample: twice(4) == 8", "{at}");
    let std = json_of(&anvil(&["std", "math", "--json"]));
    let pow = std["modules"][0]["items"].as_array().unwrap().iter().find(|i| i["name"] == "pow").unwrap().clone();
    assert!(pow["doc"].as_str().unwrap().starts_with("base to the power exp"), "{pow}");

    // The examples are tests: twice's holds, half's does not (7 / 2 is 3).
    let t = p.json(&["test", "src/maths.anv"]);
    let names: Vec<&str> = t["tests"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["twice", "example of twice on line 2", "example of half on line 13"], "{t}");
    let failing: Vec<&str> =
        t["tests"].as_array().unwrap().iter().filter(|r| r["ok"] == false).map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(failing, ["example of half on line 13"], "{t}");

    // verify: the failing example, Pair without a doc, half's stale allow.
    assert!(p.anvil(&["api", "approve", "src/maths.anv"]).status.success());
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let found: Vec<(&str, &str)> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["file"] == "src/maths.anv")
        .map(|f| (f["check"].as_str().unwrap(), f["at"].as_str().unwrap()))
        .collect();
    assert_eq!(found, [("test", ""), ("undocumented", "7:12"), ("allow", "15:8")], "{v}");
    assert!(String::from_utf8(anvil(&["explain", "undocumented"]).stdout).unwrap().starts_with("undocumented: "));

    // A broken example is a syntax error where it is written.
    p.write("src/maths.anv", "// Twice n.\n// example: twice(4) +\npub fn twice(n: i64) -> i64 {\n    return n * 2;\n}\n");
    let c = p.json(&["check", "src/maths.anv"]);
    assert_eq!(c["diagnostics"][0]["code"], "E0100", "{c}");
    assert_eq!(c["diagnostics"][0]["range"]["start"]["line"], 2, "{c}");
}

#[test]
fn public_apis_are_approved_like_goldens() {
    let p = hello_project("api");
    let shapes = "// The area of a w by h rectangle.\npub fn area(w: i64, h: i64) -> i64 {\n    return w * h;\n}\n\n\
                  test \"area\" {\n    assert(area(w: 2, h: 3) == 6);\n}\n";
    p.write("src/shapes.anv", shapes);
    p.write("src/app.anv", "import shapes.{area};\n\nfn main() uses io {\n    print(\"{area(w: 2, h: 5)}\");\n}\n");
    assert!(p.anvil(&["golden", "approve", "src/app.anv"]).status.success());
    assert!(p.anvil(&["golden", "approve", "src/hello.anv"]).status.success());
    let api_findings = |v: &serde_json::Value| -> Vec<String> {
        v["open"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["check"] == "api")
            .map(|f| f["message"].as_str().unwrap().to_string())
            .collect()
    };

    // A public API never approved is a finding; `api` lists it.
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let found = api_findings(&v);
    assert_eq!(found.len(), 1, "{v}");
    assert!(found[0].starts_with("src/shapes.anv has pub items but no approved API"), "{v}");
    let out = p.anvil(&["api", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let a = json_of(&out);
    assert_eq!(a["files"][0]["api"], serde_json::json!(["fn area(w: i64, h: i64) -> i64"]), "{a}");
    let approved = p.json(&["api", "approve", "src/shapes.anv"]);
    assert_eq!(approved["approved"], "new", "{approved}");
    assert_eq!(std::fs::read_to_string(p.0.join("golden/src/shapes.anv.api")).unwrap(), "fn area(w: i64, h: i64) -> i64\n");
    assert!(api_findings(&json_of(&p.anvil(&["verify", "--json"]))).is_empty());

    // A doc is not API; an added function is a change that breaks no one.
    p.write("src/shapes.anv", &format!("// Twice x.\npub fn double(x: i64) -> i64 {{\n    return x * 2;\n}}\n\n{}", shapes.replace("The area", "The whole area")));
    let found = api_findings(&json_of(&p.anvil(&["verify", "--json"])));
    assert_eq!(found, ["the public API added `fn double(x: i64) -> i64`; if that is meant, `anvil api approve src/shapes.anv`"]);

    // Renaming a parameter changes the API (calls name their arguments) and breaks app.anv.
    p.write("src/shapes.anv", &shapes.replace("w: i64", "width: i64").replace("w * h", "width * h").replace("area(w: 2", "area(width: 2"));
    let found = api_findings(&json_of(&p.anvil(&["verify", "--json"])));
    assert_eq!(
        found,
        ["the public API changed `fn area(w: i64, h: i64) -> i64` to `fn area(width: i64, h: i64) -> i64`; \
          it breaks the files that use it: src/app.anv; if that is meant, `anvil api approve src/shapes.anv`"]
    );
    let approved = p.json(&["api", "approve", "src/shapes.anv"]);
    assert_eq!(approved["approved"], "changed", "{approved}");
    assert_eq!(approved["changes"][0]["kind"], "changed", "{approved}");

    // With no pub items left, approving removes the approved API.
    p.write("src/shapes.anv", &shapes.replace("pub fn", "fn"));
    let approved = p.json(&["api", "approve", "src/shapes.anv"]);
    assert_eq!(approved["changes"][0]["kind"], "removed", "{approved}");
    assert!(!p.0.join("golden/src/shapes.anv.api").exists());
    assert_eq!(p.anvil(&["api", "approve", "src/shapes.anv"]).status.code(), Some(2));
}

#[test]
fn steps_bounds_are_checked_on_generated_arguments() {
    let p = hello_project("steps");
    p.write(
        "src/maths.anv",
        r#"// The total of xs.
fn total(xs: [i64]) -> i64
    steps 2 * xs.len() + 3
{
    var t: i64 = 0;
    for x in xs {
        t = t + x;
    }
    return t;
}

// Whether xs has a repeat, comparing every pair: more than its linear bound.
fn has_repeat(xs: [i64]) -> bool
    steps 4 * xs.len() + 3
{
    for i in 0..xs.len() {
        for j in 0..i {
            if xs[i] == xs[j] {
                return true;
            }
        }
    }
    return false;
}

// 0 + 1 + ... + (n - 1), one step at a time; `steps` is still a name.
fn count_to(n: i64) -> i64
    requires n >= 0
    steps 2 * n + 3
{
    var steps: i64 = 0;
    for i in 0..n {
        steps = steps + i;
    }
    return steps;
}

test "they work" {
    assert(total([1, 2]) == 3 && has_repeat([1, 1]) && count_to(4) == 6);
}
"#,
    );
    let v = json_of(&p.anvil(&["verify", "--json"]));
    let found: Vec<(&str, &str, &str)> = v["open"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == "contract")
        .map(|f| (f["check"].as_str().unwrap(), f["at"].as_str().unwrap(), f["message"].as_str().unwrap()))
        .collect();
    assert_eq!(found.len(), 1, "{v}");
    assert_eq!(found[0].1, "13:4", "{v}");
    assert!(found[0].2.contains("takes more than"), "{v}");
    assert!(found[0].2.contains("past its steps bound at src/maths.anv:14:11: 4 * xs.len() + 3"), "{v}");
    let file = v["files"].as_array().unwrap().iter().find(|f| f["file"] == "src/maths.anv").unwrap();
    let contract = |name: &str| file["contracts"].as_array().unwrap().iter().find(|c| c["function"] == name).unwrap().clone();
    assert_eq!(contract("total")["status"], "held", "{file}");
    // count_to's bound for its largest inputs is more than a check may run: those are not run.
    assert_eq!(contract("count_to")["status"], "held", "{file}");
    assert!(contract("count_to")["beyond"].as_u64().unwrap() + contract("count_to")["overflowed"].as_u64().unwrap() > 0, "{file}");
    // The bound is part of the signature: outline shows it, and fmt keeps it.
    let outline = p.json(&["outline", "src/maths.anv"]);
    assert_eq!(outline["items"][0]["signature"], "fn total(xs: [i64]) -> i64 steps 2 * xs.len() + 3", "{outline}");
    assert!(p.anvil(&["fmt", "src/maths.anv", "--check"]).status.success());

    // The bound is an i64 of the parameters, and the last clause.
    let code = |text: &str| {
        p.write("src/bad.anv", text);
        p.json(&["check", "src/bad.anv"])["diagnostics"][0]["code"].as_str().unwrap().to_string()
    };
    assert_eq!(code("fn f(n: i64) -> i64\n    steps n > 0\n{\n    return n;\n}\n"), "E0300");
    assert_eq!(code("fn f(n: i64) -> i64\n    steps result\n{\n    return n;\n}\n"), "E0322");
    assert_eq!(code("fn f(n: i64) -> i64\n    steps n\n    ensures result == n\n{\n    return n;\n}\n"), "E0100");
}

#[test]
fn cost_measures_growth_and_suggests_bounds_that_hold() {
    let p = hello_project("cost");
    p.write(
        "src/maths.anv",
        r#"// The total of xs.
fn total(xs: [i64]) -> i64 {
    var t: i64 = 0;
    for x in xs {
        t = t + x;
    }
    return t;
}

// How many pairs of xs are equal, comparing every pair.
fn equal_pairs(xs: [i64]) -> i64
    steps 40 * xs.len() + 3
{
    var n: i64 = 0;
    for i in 0..xs.len() {
        for j in 0..i {
            if xs[i] == xs[j] {
                n = n + 1;
            }
        }
    }
    return n;
}

// The larger of a and b.
fn bigger(a: i64, b: i64) -> i64 {
    if a > b {
        return a;
    }
    return b;
}

// Prints xs.
fn show(xs: [i64]) uses io {
    print("{xs.len()}");
}

test "they work" {
    assert(total([1, 2]) == 3 && equal_pairs([1, 1]) == 1 && bigger(a: 1, b: 2) == 2);
}
"#,
    );
    let v = p.json(&["cost", "src/maths.anv"]);
    let f = |name: &str| v["functions"].as_array().unwrap().iter().find(|f| f["function"] == name).unwrap().clone();
    let steps = |name: &str| f(name)["steps"].clone();
    assert_eq!((steps("total")["growth"].as_str(), steps("total")["size"].as_str()), (Some("linear"), Some("xs.len()")), "{v}");
    assert_eq!(steps("total")["suggested"], "steps 2 * xs.len() + 3", "{v}");
    assert_eq!(f("total")["bytes"]["suggested"], "allocs 0", "{v}");
    assert_eq!(steps("bigger")["growth"], "constant", "{v}");
    assert_eq!(steps("equal_pairs")["growth"], "quadratic", "{v}");
    assert_eq!(steps("equal_pairs")["outgrows_bound"], true, "its linear bound holds up to 40 items: {v}");
    assert_eq!(f("show")["skipped"], "it has effects", "{v}");

    // verify reports the bound that grows slower than the work.
    let out = json_of(&p.anvil(&["verify", "--json"]));
    let contract: Vec<&str> = out["open"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == "contract")
        .map(|f| f["message"].as_str().unwrap())
        .collect();
    assert_eq!(contract.len(), 1, "{out}");
    assert!(contract[0].contains("keeps its steps bound for every size checked, but its steps are quadratic in xs.len() and its bound only linear in xs.len()"), "{out}");

    // The suggested clauses, written into the functions, are bounds verify accepts.
    let suggested = |name: &str| f(name)["steps"]["suggested"].as_str().unwrap().to_string();
    let text = std::fs::read_to_string(p.0.join("src/maths.anv"))
        .unwrap()
        .replace("fn total(xs: [i64]) -> i64 {", &format!("fn total(xs: [i64]) -> i64\n    {}\n{{", suggested("total")))
        .replace("    steps 40 * xs.len() + 3\n", &format!("    {}\n", suggested("equal_pairs")));
    p.write("src/maths.anv", &text);
    assert!(p.anvil(&["fmt", "src/maths.anv", "--check"]).status.success(), "{text}");
    let out = json_of(&p.anvil(&["verify", "--json"]));
    assert!(out["open"].as_array().unwrap().iter().all(|f| f["check"] != "contract"), "{out}");
    let v = p.json(&["cost", "src/maths.anv", "--only", "total"]);
    assert_eq!(v["functions"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(p.anvil(&["cost", "src/maths.anv", "--only", "nope"]).status.code(), Some(2));
}

#[test]
fn memory_is_counted_bounded_and_measured() {
    let p = hello_project("memory");
    p.write(
        "src/maths.anv",
        r#"// xs with 1 after it: xs is shared with the caller, so the push copies it first.
fn grow(xs: [i64]) -> [i64]
    allocs 8 * xs.len() + 8
{
    var ys: [i64] = xs;
    ys.push(1);
    return ys;
}

// "a" n times, each put before all that came before: each copies it all.
fn dashes(n: i64) -> str
    requires n >= 0
    allocs 2 * n
{
    var out: str = "";
    for _i in 0..n {
        out = "a" + out;
    }
    return out;
}

// "a" n times, each put after: the text grows in place into room that doubles.
fn tail(n: i64) -> str
    requires n >= 0
    allocs 3 * n
{
    var out: str = "";
    for _i in 0..n {
        out = out + "a";
    }
    return out;
}

// n ones, each added by a call that is handed the only reference: nothing is copied.
fn stack(n: i64) -> [i64]
    requires n >= 0
    allocs 8 * n
{
    var xs: [i64] = [];
    for _i in 0..n {
        xs = grow(xs);
    }
    return xs;
}

struct Bag {
    items: [i64],
}

// n ones in a field, each added by a call handed the field's only reference.
fn bagged(n: i64) -> Bag
    requires n >= 0
    allocs 8 * n
{
    var b: Bag = Bag { items: [] };
    for _i in 0..n {
        b.items = grow(b.items);
    }
    return b;
}

// n ones in the first of two arrays, each added in place the same way.
fn rows(n: i64) -> [[i64]]
    requires n >= 0
    allocs 8 * n + 16
{
    var r: [[i64]] = [[], []];
    for _i in 0..n {
        r[0] = grow(r[0]);
    }
    return r;
}

// s with a mark after it: handed s, it appends in place.
fn bang(s: str) -> str {
    return s + "!";
}

// n marks, each added by a call handed the text: linear, as `out = out + "!"` is.
fn bangs(n: i64) -> str
    requires n >= 0
    allocs 3 * n
{
    var out: str = "";
    for _i in 0..n {
        out = bang(out);
    }
    return out;
}

// "abcdef" has no room to grow, so the first append copies it: 6 + (6 + the end).
fn once(end: str) -> str {
    let a: str = "abc" + "def";
    var s: str = a;
    s = s + end;
    return s;
}

// n, in one step more than its bound says.
fn same(n: i64) -> i64
    steps 0
{
    return n;
}

test "they work" {
    assert(grow([2]) == [2, 1] && dashes(3) == "aaa" && tail(3) == "aaa" && stack(2) == [1, 1] && same(4) == 4);
    assert(bagged(2).items == [1, 1] && rows(1) == [[1], []]);
    assert(bangs(2) == "!!" && once("g") == "abcdefg");
}
"#,
    );
    // cost: a shared array is copied before the push; concatenating one at a time is triangular.
    let v = p.json(&["cost", "src/maths.anv"]);
    let f = |name: &str| v["functions"].as_array().unwrap().iter().find(|f| f["function"] == name).unwrap().clone();
    assert_eq!(f("grow")["bytes"]["suggested"], "allocs 8 * xs.len() + 8", "{v}");
    assert_eq!(f("grow")["bytes"]["bound_holds"], true, "{v}");
    assert_eq!(f("dashes")["bytes"]["growth"], "quadratic", "{v}");
    assert_eq!(f("dashes")["bytes"]["suggested"], "allocs n * (n + 1) / 2", "{v}");
    assert_eq!(f("dashes")["bytes"]["bound_holds"], false, "{v}");
    // Appending grows in place: linear, within its bound of three bytes a character.
    assert_eq!(f("tail")["bytes"]["growth"], "linear", "{v}");
    assert_eq!(f("tail")["bytes"]["bound_holds"], true, "{v}");
    // Handed over on its last read, the array is pushed to in place: linear, not quadratic.
    assert_eq!(f("stack")["bytes"]["growth"], "linear", "{v}");
    assert_eq!(f("stack")["bytes"]["suggested"], "allocs 8 * n", "{v}");
    assert_eq!(f("stack")["bytes"]["bound_holds"], true, "{v}");
    // Text handed to a call and given back grows in place; the first append to text without room
    // copies it, as the native code does.
    assert_eq!(f("bangs")["bytes"]["growth"], "linear", "{v}");
    assert_eq!(f("bangs")["bytes"]["bound_holds"], true, "{v}");
    assert_eq!(f("once")["bytes"]["suggested"], "allocs end.len() + 12", "{v}");
    // The same in a field and in an element: taken on the last read, so linear too.
    for (name, clause) in [("bagged", "allocs 8 * n"), ("rows", "allocs 8 * n + 16")] {
        assert_eq!(f(name)["bytes"]["growth"], "linear", "{name}: {v}");
        assert_eq!(f(name)["bytes"]["suggested"], clause, "{name}: {v}");
        assert_eq!(f(name)["bytes"]["bound_holds"], true, "{name}: {v}");
    }
    assert_eq!(f("same")["steps"]["bound_holds"], false, "one step past `steps 0`: {v}");

    // verify: the allocs bound and the steps bound one step short, each shrunk.
    let out = json_of(&p.anvil(&["verify", "--json"]));
    let found: Vec<(&str, &str)> = out["open"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == "contract")
        .map(|f| (f["at"].as_str().unwrap(), f["message"].as_str().unwrap()))
        .collect();
    assert_eq!(found.len(), 2, "{out}");
    assert!(found[0].1.starts_with("`dashes` breaks its contract for n: 4: allocates 10 bytes, more than 8"), "{out}");
    assert!(found[1].1.starts_with("`same` breaks its contract for n: 0: takes more than 0 steps"), "{out}");

    // profile shows the bytes each function allocated.
    p.write(
        "src/hello.anv",
        "// xs with 1 after it.\nfn grow(xs: [i64]) -> [i64] {\n    var ys: [i64] = xs;\n    ys.push(1);\n    return ys;\n}\n\n\
         fn main() uses io {\n    let ys: [i64] = grow([1, 2, 3]);\n    print(\"{ys.len()}\");\n}\n",
    );
    let prof = p.json(&["profile", "src/hello.anv"]);
    let row = |name: &str| prof["functions"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap().clone();
    // grow is given main's literal, which nothing else holds, and moves it into ys, so the
    // push (8) needs no copy; main makes the literal (24) and the interpolated "4" (1); the
    // format text itself is static.
    assert_eq!((row("grow")["bytes"].as_u64(), row("grow")["self_bytes"].as_u64()), (Some(8), Some(8)), "{prof}");
    assert_eq!((row("main")["bytes"].as_u64(), row("main")["self_bytes"].as_u64()), (Some(33), Some(25)), "{prof}");
    assert_eq!(prof["bytes"], 33, "{prof}");
    assert_eq!((prof["copied"].as_u64(), row("grow")["self_copied"].as_u64()), (Some(0), Some(0)), "{prof}");

    // Kept by main after the call, xs is shared when grow pushes to it: copied (24), and said so.
    p.write(
        "src/hello.anv",
        "// xs with 1 after it.\nfn grow(xs: [i64]) -> [i64] {\n    var ys: [i64] = xs;\n    ys.push(1);\n    return ys;\n}\n\n\
         fn main() uses io {\n    let xs: [i64] = [1, 2, 3];\n    let ys: [i64] = grow(xs);\n    print(\"{xs.len()} {ys.len()}\");\n}\n",
    );
    let prof = p.json(&["profile", "src/hello.anv"]);
    let row = |name: &str| prof["functions"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap().clone();
    assert_eq!((row("grow")["self_bytes"].as_u64(), row("grow")["self_copied"].as_u64()), (Some(32), Some(24)), "{prof}");
    assert_eq!(prof["copied"], 24, "{prof}");
    let text = String::from_utf8_lossy(&p.anvil(&["profile", "src/hello.anv"]).stdout).to_string();
    assert!(text.contains("24 of those bytes copied values still shared"), "{text}");
    // ... and where: the push in grow, handed the shared xs by the call in main.
    let copies = prof["copies"].as_array().unwrap();
    assert_eq!(copies.len(), 1, "{prof}");
    let c = &copies[0];
    assert!(c["at"].as_str().unwrap().ends_with("hello.anv:4:5"), "{prof}");
    assert_eq!((c["function"].as_str(), c["line"].as_str()), (Some("grow"), Some("ys.push(1);")), "{prof}");
    assert_eq!((c["bytes"].as_u64(), c["times"].as_u64()), (Some(24), Some(1)), "{prof}");
    let from = &c["from"][0];
    assert!(from["at"].as_str().unwrap().ends_with("hello.anv:10:21"), "{prof}");
    assert_eq!(from["line"], "let ys: [i64] = grow(xs);", "{prof}");
    // ... and why: xs is not moved into the call, as main reads it again.
    assert_eq!(from["args"], serde_json::json!([{ "value": "xs", "moved": false, "kept": "it is read again later" }]), "{prof}");
    assert!(text.contains("`xs` is not moved there: it is read again later"), "{text}");
    assert!(text.contains("in grow: ys.push(1);") && text.contains(":10:21: let ys: [i64] = grow(xs);"), "{text}");

    // The clauses are ordered after requires and ensures, one of each.
    let code = |text: &str| {
        p.write("src/bad.anv", text);
        p.json(&["check", "src/bad.anv"])["diagnostics"][0]["code"].as_str().unwrap().to_string()
    };
    assert_eq!(code("fn f(n: i64) -> i64\n    allocs 0\n    allocs 1\n{\n    return n;\n}\n"), "E0100");
    assert_eq!(code("fn f(n: i64) -> i64\n    allocs 0\n    requires n > 0\n{\n    return n;\n}\n"), "E0100");
    assert_eq!(code("fn f(n: i64) -> i64\n    allocs true\n{\n    return n;\n}\n"), "E0300");
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
    assert!(p.anvil(&["usage", "approve"]).status.success());
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
        "// s, shouted.\npub fn shout(s: str) -> str {\n    return s + \"!\";\n}\n\n// s, quietly.\npub fn whisper(s: str) -> str {\n    return s;\n}\n\n\
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
    assert!(p.anvil(&["api", "approve", "src/text/words.anv"]).status.success());
    assert!(p.anvil(&["usage", "approve"]).status.success());
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
    assert!(p.anvil(&["usage", "approve"]).status.success());
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

#[test]
fn usage_is_approved_and_growth_is_found() {
    let p = hello_project("usage");
    assert!(p.anvil(&["golden", "approve", "src/hello.anv"]).status.success());
    let usage_findings = |v: &serde_json::Value| -> Vec<String> {
        v["open"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["check"] == "usage")
            .map(|f| f["message"].as_str().unwrap().to_string())
            .collect()
    };
    let twice = "fn twice(n: i64) -> i64 {\n    return n * 2;\n}\n\ntest \"twice\" {\n    assert(twice(2) == 4);\n}\n";

    // Never approved: a finding for each file that runs, saying how to approve it.
    let found = usage_findings(&json_of(&p.anvil(&["verify", "--json"])));
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[1].starts_with("src/maths.anv has no approved usage; check it with `anvil usage src/maths.anv`"), "{found:?}");
    let out = p.anvil(&["usage", "src/maths.anv", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let u = json_of(&out);
    let run = &u["files"][0]["runs"][0];
    assert_eq!(run["run"], "test \"twice\"", "{u}");
    let steps = run["steps"].as_u64().unwrap();
    assert!(steps > 0, "{u}");
    let approved = p.json(&["usage", "approve"]);
    assert_eq!(approved["approved"].as_array().unwrap().len(), 2, "{approved}");
    let file = std::fs::read_to_string(p.0.join("golden/src/maths.anv.usage")).unwrap();
    assert_eq!(file, format!("test \"twice\": {steps} steps, 0 bytes, 0 copied\n"));
    assert!(usage_findings(&json_of(&p.anvil(&["verify", "--json"]))).is_empty());
    assert!(p.anvil(&["usage"]).status.success());

    // More work: a finding with the numbers, and how to accept it.
    p.write("src/maths.anv", &twice.replace("return n * 2;", "var r: i64 = 0;\n    for _i in 0..2 {\n        r = r + n;\n    }\n    return r;"));
    let found = usage_findings(&json_of(&p.anvil(&["verify", "--json"])));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with(&format!("usage test \"twice\": {steps} -> ")), "{found:?}");
    assert!(found[0].contains(" steps (+"), "{found:?}");
    assert!(found[0].ends_with("if that is meant, `anvil usage approve src/maths.anv`"), "{found:?}");
    assert_eq!(p.anvil(&["usage"]).status.code(), Some(1));
    assert_eq!(p.json(&["usage", "approve", "src/maths.anv"])["approved"][0]["approved"], "changed");

    // Less: listed, not a finding; approving keeps the gain.
    p.write("src/maths.anv", twice);
    let out = p.anvil(&["usage", "src/maths.anv"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("(less)") && text.contains("approve it to keep the gain"), "{text}");
    assert!(usage_findings(&json_of(&p.anvil(&["verify", "--json"]))).is_empty());
    assert!(p.anvil(&["usage", "approve", "src/maths.anv"]).status.success());

    // A new test is not approved yet; a failing one is not measured, and cannot be approved.
    p.write("src/maths.anv", &format!("{twice}\ntest \"again\" {{\n    assert(twice(3) == 6);\n}}\n"));
    let found = usage_findings(&json_of(&p.anvil(&["verify", "--json"])));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("usage test \"again\": ") && found[0].contains("is not approved yet"), "{found:?}");
    p.write("src/maths.anv", &format!("{twice}\ntest \"again\" {{\n    assert(twice(3) == 7);\n}}\n"));
    let out = p.anvil(&["usage", "approve", "src/maths.anv"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("test \"again\" cannot be measured, as it fails"), "{out:?}");
}
