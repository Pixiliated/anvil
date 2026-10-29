//! End-to-end tests through the real `anvil` binary.
//!
//! tests/programs: each program has a golden NAME.stdout plus `// exit:` and `// panic:` headers.
//! The interpreter and the native executable must both match the golden exactly.
//! Set ANVIL_BLESS=1 to write missing goldens from the interpreter; review them before committing.
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
    Command::new(env!("CARGO_BIN_EXE_anvil")).args(args).current_dir(root()).output().unwrap()
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

struct Expected {
    exit: i32,
    stderr: String,
    stdout: String,
}

fn expected(rel: &str) -> Expected {
    let text = std::fs::read_to_string(root().join(rel)).unwrap();
    let exit = header(&text, "exit").map(|s| s.parse().unwrap()).unwrap_or(0);
    let stderr = header(&text, "panic").map(|m| format!("panic: {m}\n")).unwrap_or_default();
    let golden = root().join(rel).with_extension("stdout");
    let stdout = match std::fs::read_to_string(&golden) {
        Ok(s) => s,
        Err(_) if std::env::var("ANVIL_BLESS").is_ok() => {
            let out = anvil(&["run", rel]);
            std::fs::write(&golden, &out.stdout).unwrap();
            String::from_utf8(out.stdout).unwrap()
        }
        Err(_) => panic!("{} has no golden {}; run with ANVIL_BLESS=1 and review it", rel, golden.display()),
    };
    if exit != 101 {
        assert!(stderr.is_empty(), "{rel}: a panic header needs `// exit: 101`");
    }
    Expected { exit, stderr, stdout }
}

fn compare(rel: &str, how: &str, out: &Output, want: &Expected) -> Vec<String> {
    let mut problems = Vec::new();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stdout != want.stdout {
        problems.push(format!("{rel} ({how}): stdout differs\n--- want\n{}--- got\n{stdout}", want.stdout));
    }
    if stderr != want.stderr {
        problems.push(format!("{rel} ({how}): stderr differs\n  want {:?}\n  got  {stderr:?}", want.stderr));
    }
    if out.status.code() != Some(want.exit) {
        problems.push(format!("{rel} ({how}): exit {:?}, want {}", out.status.code(), want.exit));
    }
    problems
}

#[test]
fn interpreter_matches_goldens() {
    let mut problems = Vec::new();
    for rel in files("tests/programs") {
        let want = expected(&rel);
        problems.extend(compare(&rel, "interpreter", &anvil(&["run", &rel]), &want));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn native_matches_goldens() {
    let dir = std::env::temp_dir().join(format!("anvil_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut problems = Vec::new();
    for rel in files("tests/programs") {
        let want = expected(&rel);
        let exe = dir.join(Path::new(&rel).with_extension("exe").file_name().unwrap());
        let build = anvil(&["build", &rel, "-o", exe.to_str().unwrap()]);
        assert!(build.status.success(), "{rel}: build failed: {}", String::from_utf8_lossy(&build.stderr));
        let out = Command::new(&exe).current_dir(root()).output().unwrap();
        problems.extend(compare(&rel, "native", &out, &want));
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
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
    let no_main = anvil(&["run", "tests/test_blocks.anv", "--json"]);
    assert_eq!(no_main.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&no_main.stdout).contains("E0350"));
}
