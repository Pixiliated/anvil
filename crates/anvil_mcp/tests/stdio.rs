//! Drives the real `anvil-mcp` binary over stdio JSON-RPC, the way an MCP client does.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_anvil-mcp"))
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawning anvil-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Client { child, stdin, stdout, next_id: 0 };
        let init = c.request(
            "initialize",
            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "anvil", "{init}");
        assert!(init["result"]["capabilities"]["tools"].is_object(), "{init}");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("spec"), "{init}");
        c.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        c
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "anvil-mcp closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    /// Calls a tool; returns (is_error, the JSON it sent back).
    fn call(&mut self, name: &str, args: Value) -> (bool, Value) {
        let v = self.request("tools/call", json!({ "name": name, "arguments": args }));
        let r = &v["result"];
        assert!(r.is_object(), "{name}: no result: {v}");
        let text = r["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{name}: no text: {v}"));
        let json = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: not JSON ({e}): {text}"));
        (r["isError"] == true, json)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

#[test]
fn tools_answer_over_stdio() {
    let mut c = Client::start(&repo());

    let list = c.request("tools/list", json!({}));
    let names: Vec<&str> =
        list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for want in ["check", "run", "build", "test", "fix", "outline", "type_at", "refs", "explain", "spec"] {
        assert!(names.contains(&want), "missing tool {want}: {names:?}");
    }

    let (err, v) = c.call("check", json!({ "file": "examples/orders.anv" }));
    assert!(!err && v["ok"] == true, "{v}");

    let (err, v) = c.call("check", json!({ "file": "tests/errors/E0400_missing_arm.anv" }));
    assert!(err, "a failing check is a tool error: {v}");
    assert_eq!(v["diagnostics"][0]["code"], "E0400");

    let (err, v) = c.call("run", json!({ "file": "examples/orders.anv" }));
    assert!(!err, "{v}");
    assert_eq!(v["exit"], 0);
    assert!(v["stdout"].as_str().unwrap().starts_with("total $69.98\n"), "{v}");

    let (err, v) = c.call("run", json!({ "file": "tests/programs/panic_index.anv" }));
    assert!(err, "{v}");
    assert!(v["panic"].as_str().unwrap().starts_with("index 3 out of bounds"), "{v}");
    assert_eq!(v["stdout"], "0: 10\n1: 20\n2: 30\n");

    let (err, v) = c.call("run", json!({ "file": "tests/programs/panic_index.anv", "max_steps": 5 }));
    assert!(err, "{v}");
    assert!(v["stopped"].as_str().unwrap().contains("step limit"), "{v}");

    let (err, v) = c.call("test", json!({ "file": "tests/test_blocks.anv" }));
    assert!(err, "a failing test is a tool error: {v}");
    assert_eq!(v["passed"], 2);

    let (err, v) = c.call("outline", json!({ "file": "examples/orders.anv" }));
    assert!(!err, "{v}");
    assert!(v["items"].as_array().unwrap().iter().any(|i| i["name"] == "order_total"), "{v}");

    let (err, v) = c.call("refs", json!({ "file": "examples/orders.anv", "name": "Shipping.Courier" }));
    assert!(!err, "{v}");
    assert_eq!(v["declarations"][0]["kind"], "variant");

    let (err, v) = c.call("explain", json!({ "code": "E0405" }));
    assert!(!err && v["explanation"].as_str().unwrap().contains('?'), "{v}");

    let (err, v) = c.call("spec", json!({}));
    assert!(!err && v["spec"].as_str().unwrap().starts_with("# Anvil language specification"), "{v}");

    // Unknown arguments are refused, not ignored.
    let bad = c.request("tools/call", json!({ "name": "check", "arguments": { "file": "x.anv", "verbose": true } }));
    assert!(bad["error"].is_object() || bad["result"]["isError"] == true, "{bad}");
}

#[test]
fn fix_and_build_write_files() {
    let dir = std::env::temp_dir().join(format!("anvil_mcp_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f.anv"), "fn main() uses io {\n    let x: i64 = 4;\n    print(\"x = \", x)\n}\n").unwrap();
    let mut c = Client::start(&dir);
    let (err, v) = c.call("fix", json!({ "file": "f.anv" }));
    assert!(!err, "{v}");
    assert_eq!(v["applied"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.anv")).unwrap(),
        "fn main() uses io {\n    let x: i64 = 4;\n    print(\"x = {x}\");\n}\n"
    );
    let (err, v) = c.call("build", json!({ "file": "f.anv", "out": "f.exe" }));
    assert!(!err, "{v}");
    let out = std::process::Command::new(dir.join("f.exe")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "x = 4\n");
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_tools_answer_over_stdio() {
    let dir = std::env::temp_dir().join(format!("anvil_mcp_project_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join("anvil.toml"), "[project]\nname = \"p\"\nfiles = [\"src/*.anv\"]\n").unwrap();
    std::fs::write(dir.join("src/main.anv"), "fn main() uses io {\n    print(\"hi\");\n}\n").unwrap();
    std::fs::write(dir.join("docs/ROADMAP.md"), "## M1: First\n- [ ] Start\n").unwrap();
    let mut c = Client::start(&dir);

    let (err, v) = c.call("list", json!({}));
    assert!(!err && v["files"][0]["file"] == "src/main.anv", "{v}");
    let (err, v) = c.call("golden_check", json!({}));
    assert!(err && v["results"][0]["status"] == "missing", "{v}");
    let (err, v) = c.call("golden_approve", json!({ "file": "src/main.anv" }));
    assert!(!err && v["stdout"] == "hi\n", "{v}");
    let (err, v) = c.call("golden_check", json!({ "native": true }));
    assert!(!err && v["native"] == true, "{v}");
    let (err, v) = c.call("verify", json!({}));
    assert!(!err && v["findings"]["open"] == 0, "{v}");
    let (err, v) = c.call("record_import_roadmap", json!({}));
    assert!(!err && v["current"]["id"] == "M1", "{v}");
    let (err, _) = c.call("record_decide", json!({ "title": "Use goldens" }));
    assert!(!err);
    let (err, v) = c.call("record_query", json!({}));
    assert!(!err && v["decisions"] == 1 && v["last_run"]["open"] == 0, "{v}");
    let (err, v) = c.call("record_status", json!({ "milestone": "M1", "status": "finished" }));
    assert!(err && v["error"].as_str().unwrap().contains("open, active, done, dropped"), "{v}");
    let (err, v) = c.call("review", json!({}));
    assert!(!err && v["markdown"].as_str().unwrap().contains("## Sign-off"), "{v}");
    let (err, v) = c.call("record_export", json!({}));
    assert!(!err && v["markdown"].as_str().unwrap().contains("Use goldens"), "{v}");

    // Watching a run: the same JSON as the CLI.
    let (err, v) = c.call("trace", json!({ "file": "src/main.anv" }));
    assert!(!err && v["events"][1]["print"] == "hi" && v["end"]["exit"] == 0, "{v}");
    let (err, v) = c.call("profile", json!({ "file": "src/main.anv", "max_steps": 0 }));
    assert!(!err && v["end"]["stopped"].as_str().unwrap().contains("after 0 steps"), "{v}");
    let (err, v) = c.call("trace", json!({ "file": "src/main.anv", "test": "none" }));
    assert!(err && v["error"].as_str().unwrap().contains("it has none"), "{v}");
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}
