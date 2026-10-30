//! End-to-end tests through the real `anvil` binary.
//!
//! The repository is itself an Anvil project (anvil.toml): every program in tests/programs and
//! examples has an approved golden in golden/, and `anvil golden check --native` must pass, so
//! the interpreter and the native executable agree with each other and with the approved output.
//!
//! tests/errors: each file lists `// expect: CODE...`; `// resolve: mechanical` means applying
//! every mechanical fix must give a program that checks cleanly.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    let bad: Vec<_> = v["results"].as_array().unwrap().iter().filter(|r| r["status"] != "ok").collect();
    assert!(bad.is_empty() && out.status.success(), "{bad:#?}");
    // Every program of the corpus is in the project and has a main.
    assert_eq!(v["checked"].as_u64().unwrap() as usize, files("tests/programs").len() + files("examples").len());
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
    let a = dir.join("a.exe");
    let b = dir.join("b.exe");
    for exe in [&a, &b] {
        let out = anvil(&["build", "examples/projectile.anv", "-o", exe.to_str().unwrap()]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let same = std::fs::read(&a).unwrap() == std::fs::read(&b).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(same, "two builds of the same source differ");
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
    std::fs::create_dir_all(&tmp).unwrap();
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
            let fixed = apply_mechanical(&text, &json);
            let path = tmp.join(Path::new(&rel).file_name().unwrap());
            std::fs::write(&path, &fixed).unwrap();
            let (ok, after) = check_json(path.to_str().unwrap());
            if !ok {
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
    let all: BTreeSet<String> =
        all.as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap().to_string()).collect();
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
        let test = anvil(&["test", &rel, "--json"]);
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

    let binding = json_of(&anvil(&["type-at", f, "55", &col_of(f, 55, "max_weight"), "--json"]));
    assert_eq!(binding["type"], "f64[kg]", "{binding}");
    assert_eq!(binding["declaration"]["kind"], "local", "{binding}");
    assert_eq!(binding["declaration"]["defined_at"]["line"], 54, "{binding}");

    // Inside an interpolation: the innermost expression is the call, the name is the function.
    let call = json_of(&anvil(&["type-at", f, "89", &col_of(f, 89, "money"), "--json"]));
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
    assert_eq!(lines, vec![44, 100, 101, 102], "{weight}");
    let courier = json_of(&anvil(&["refs", f, "Shipping.Courier", "--json"]));
    let lines: Vec<i64> =
        courier["declarations"][0]["uses"].as_array().unwrap().iter().map(|u| u["line"].as_i64().unwrap()).collect();
    assert_eq!(lines, vec![54, 109, 111], "{courier}");
    let unit = json_of(&anvil(&["refs", f, "kg", "--json"]));
    assert_eq!(unit["declarations"][0]["kind"], "unit", "{unit}");

    let missing = anvil(&["refs", f, "mony", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
    let v = json_of(&missing);
    assert!(v["declared"].as_array().unwrap().iter().any(|n| n == "money"), "{v}");
    assert_eq!(v["suggestion"], "money", "{v}");
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
