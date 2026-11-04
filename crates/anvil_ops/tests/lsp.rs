//! The language server, driven by a scripted session: every request goes in at once, then
//! the answers and notifications that came out are read back in order.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const GEO: &str = "pub struct Point {\n    x: i64,\n    y: i64,\n}\n\n// The squared length of p.\npub fn norm2(p: Point) -> i64 {\n    return p.x * p.x + p.y * p.y;\n}\n";
// The emoji takes two UTF-16 units and one character, so the encodings give different columns.
const MAIN: &str = "import lib.geo.{Point, norm2};\n\nfn main() uses io {\n    let p: Point = Point { x: 3, y: 4 };\n    print(\"\u{1F600} {norm2(p)} {p.x}\");\n}\n";

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("anvil_lsp_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("anvil.toml"), "[project]\nname = \"lsp\"\nfiles = [\"*.anv\", \"lib/*.anv\"]\n")
            .unwrap();
        std::fs::write(dir.join("lib/geo.anv"), GEO).unwrap();
        std::fs::write(dir.join("main.anv"), MAIN).unwrap();
        Scratch(dir)
    }

    fn uri(&self, rel: &str) -> String {
        let path = self.0.join(rel).to_string_lossy().replace('\\', "/");
        let path = path.replace(' ', "%20");
        if path.starts_with('/') {
            format!("file://{path}")
        } else {
            format!("file:///{path}")
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// Runs a session; the report's JSON and every message the server sent.
fn session(messages: &[Value]) -> (anvil_ops::Report, Vec<Value>) {
    let input: Vec<u8> = messages.iter().flat_map(frame).collect();
    let mut output = Vec::new();
    let report = anvil_ops::lsp(&mut &input[..], &mut output);
    let mut out = Vec::new();
    let mut rest = &output[..];
    while !rest.is_empty() {
        let text = std::str::from_utf8(rest).unwrap();
        let header_end = text.find("\r\n\r\n").expect("a header");
        let n: usize = text[..header_end].strip_prefix("Content-Length: ").unwrap().parse().unwrap();
        let body = &rest[header_end + 4..header_end + 4 + n];
        out.push(serde_json::from_slice(body).unwrap());
        rest = &rest[header_end + 4 + n..];
    }
    (report, out)
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notify(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn answer(out: &[Value], id: u64) -> &Value {
    out.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no answer to request {id}: {out:#?}"))
}

fn published<'a>(out: &'a [Value], uri: &str) -> Vec<&'a Value> {
    out.iter().filter(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri).collect()
}

/// The UTF-16 column of `needle` in `line` of MAIN.
fn utf16_col(line: usize, needle: &str) -> usize {
    let text = MAIN.lines().nth(line).unwrap();
    text[..text.find(needle).unwrap()].encode_utf16().count()
}

fn at(uri: &str, line: usize, character: usize) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } })
}

fn open(uri: &str, text: &str) -> Value {
    notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "anvil", "version": 1, "text": text } }),
    )
}

fn initialize(root: &str, encodings: &[&str]) -> Value {
    request(
        1,
        "initialize",
        json!({ "rootUri": root, "capabilities": { "general": { "positionEncodings": encodings } } }),
    )
}

#[test]
fn answers_come_from_the_same_facts_as_the_commands() {
    let s = Scratch::new("facts");
    let main = s.uri("main.anv");
    let x_col = utf16_col(4, "x}");
    let (report, out) = session(&[
        initialize(&s.uri(""), &["utf-16"]),
        notify("initialized", json!({})),
        open(&main, MAIN),
        request(2, "textDocument/hover", at(&main, 4, utf16_col(4, "norm2"))),
        request(3, "textDocument/definition", at(&main, 4, utf16_col(4, "norm2"))),
        request(4, "textDocument/references", {
            let mut p = at(&main, 3, 8);
            p["context"] = json!({ "includeDeclaration": true });
            p
        }),
        request(5, "textDocument/completion", at(&main, 4, x_col)),
        request(6, "textDocument/documentSymbol", json!({ "textDocument": { "uri": main } })),
        request(7, "textDocument/rename", at(&main, 3, 8)),
        request(8, "shutdown", Value::Null),
        notify("exit", Value::Null),
    ]);
    assert!(!report.failed, "{}", report.json);

    let init = &answer(&out, 1)["result"];
    assert_eq!(init["capabilities"]["positionEncoding"], "utf-16");
    assert_eq!(init["capabilities"]["completionProvider"]["triggerCharacters"], json!(["."]));

    // A clean file publishes an empty list.
    let diags = published(&out, &main);
    assert_eq!(diags.len(), 1, "{out:#?}");
    assert_eq!(diags[0]["params"]["diagnostics"], json!([]));

    let hover = answer(&out, 2)["result"]["contents"]["value"].as_str().unwrap().to_string();
    assert!(hover.contains("norm2(p): i64"), "{hover}");
    assert!(hover.contains("fn `norm2` declared at lib/geo.anv:7:8"), "{hover}");
    assert!(hover.ends_with("The squared length of p."), "the doc comment comes last: {hover}");

    // Into the module, at the name: `pub fn norm2` is line 7 (index 6), the name at column 8.
    let def = &answer(&out, 3)["result"];
    assert!(def["uri"].as_str().unwrap().ends_with("/lib/geo.anv"), "{def}");
    assert_eq!(def["range"]["start"], json!({ "line": 6, "character": 7 }));
    assert_eq!(def["range"]["end"], json!({ "line": 6, "character": 12 }));

    // The local `p`: its declaration and both uses, the second after the emoji.
    let refs = answer(&out, 4)["result"].as_array().unwrap().clone();
    let starts: Vec<(u64, u64)> = refs
        .iter()
        .map(|l| (l["range"]["start"]["line"].as_u64().unwrap(), l["range"]["start"]["character"].as_u64().unwrap()))
        .collect();
    let p_uses = (utf16_col(4, "p)") as u64, utf16_col(4, "p.x") as u64);
    assert_eq!(starts, vec![(3, 8), (4, p_uses.0), (4, p_uses.1)], "{refs:#?}");
    assert!(refs.iter().all(|l| l["uri"] == main));

    // After `p.`: the fields of Point, with their types.
    let items = answer(&out, 5)["result"]["items"].as_array().unwrap().clone();
    let labels: Vec<&str> = items.iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["x", "y"], "{items:#?}");
    assert_eq!(items[0]["detail"], "i64");
    assert_eq!(items[0]["kind"], 5);

    let symbols = answer(&out, 6)["result"].as_array().unwrap().clone();
    let names: Vec<&str> = symbols.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["import lib.geo", "main"]);

    // Unknown requests are errors, not silence.
    assert_eq!(answer(&out, 7)["error"]["code"], -32601);
    assert_eq!(answer(&out, 8)["result"], Value::Null);
}

#[test]
fn edits_republish_and_fixes_and_formatting_come_back_as_edits() {
    let s = Scratch::new("edits");
    let main = s.uri("main.anv");
    // Not in canonical layout (E0110), and calling a function that does not exist (E0200).
    let messy = MAIN.replace("let p: Point =", "let p: Point  =");
    let wrong = MAIN.replace("{norm2(p)}", "{norm3(p)}");
    let (report, out) = session(&[
        initialize(&s.uri(""), &["utf-32", "utf-16"]),
        open(&main, MAIN),
        notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": main, "version": 2 }, "contentChanges": [{ "text": messy }] }),
        ),
        request(
            2,
            "textDocument/formatting",
            json!({ "textDocument": { "uri": main }, "options": { "tabSize": 4, "insertSpaces": true } }),
        ),
        request(
            3,
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": main },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } }, "context": { "diagnostics": [] } }),
        ),
        notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": main, "version": 3 }, "contentChanges": [{ "text": wrong }] }),
        ),
        // UTF-32: the column is the character count, the emoji counting once.
        request(
            4,
            "textDocument/completion",
            at(&main, 4, MAIN.lines().nth(4).unwrap().chars().position(|c| c == 'x').unwrap()),
        ),
        notify("textDocument/didClose", json!({ "textDocument": { "uri": main } })),
        request(5, "shutdown", Value::Null),
        notify("exit", Value::Null),
    ]);
    assert!(!report.failed, "{}", report.json);
    assert_eq!(answer(&out, 1)["result"]["capabilities"]["positionEncoding"], "utf-32");

    let lists: Vec<Value> = published(&out, &main).iter().map(|m| m["params"]["diagnostics"].clone()).collect();
    let codes: Vec<Vec<&str>> =
        lists.iter().map(|l| l.as_array().unwrap().iter().map(|d| d["code"].as_str().unwrap()).collect()).collect();
    // Open (clean), the messy change, the wrong name (which leaves norm2's import unused), then
    // cleared on close.
    assert_eq!(codes, vec![vec![], vec!["E0110"], vec!["E0604", "E0200"], vec![]], "{lists:#?}");
    let e0200 = &lists[2][1];
    let line = MAIN.lines().nth(4).unwrap();
    let col = line[..line.find("norm2").unwrap()].chars().count();
    assert_eq!(e0200["range"]["start"], json!({ "line": 4, "character": col }));

    // Formatting gives back the canonical text as one edit.
    let edits = answer(&out, 2)["result"].as_array().unwrap().clone();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["newText"], MAIN);

    // The layout error's fix is offered as the preferred quick fix.
    let actions = answer(&out, 3)["result"].as_array().unwrap().clone();
    assert!(
        actions.iter().any(|a| a["isPreferred"] == true && a["title"].as_str().unwrap().contains("E0110")),
        "{actions:#?}"
    );
    let edit = &actions[0]["edit"]["changes"][&main];
    assert!(edit.is_array(), "{actions:#?}");

    let items = answer(&out, 4)["result"]["items"].as_array().unwrap().clone();
    assert_eq!(items.iter().map(|i| i["label"].as_str().unwrap()).collect::<Vec<_>>(), ["x", "y"]);
}

#[test]
fn errors_in_an_imported_module_are_published_to_that_module() {
    let s = Scratch::new("module");
    std::fs::write(s.0.join("lib/geo.anv"), GEO.replace("p.y * p.y", "p.y * p.z")).unwrap();
    let main = s.uri("main.anv");
    let (_, out) = session(&[
        initialize(&s.uri(""), &[]),
        open(&main, MAIN),
        request(2, "shutdown", Value::Null),
        notify("exit", Value::Null),
    ]);
    let geo: Vec<&Value> = out
        .iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .filter(|m| m["params"]["uri"].as_str().unwrap().ends_with("/lib/geo.anv"))
        .collect();
    assert_eq!(geo.len(), 1, "{out:#?}");
    assert_eq!(geo[0]["params"]["diagnostics"][0]["code"], "E0306");
    assert_eq!(geo[0]["params"]["diagnostics"][0]["range"]["start"]["line"], 7);
}

#[test]
fn the_protocol_is_kept() {
    let s = Scratch::new("protocol");
    let main = s.uri("main.anv");
    // exit without shutdown is a failure, as the protocol says.
    let (report, _) = session(&[initialize(&s.uri(""), &[]), notify("exit", Value::Null)]);
    assert!(report.failed);
    // Requests about a file that is not open are errors.
    let (_, out) = session(&[
        initialize(&s.uri(""), &[]),
        request(2, "textDocument/hover", at(&main, 0, 0)),
        request(3, "shutdown", Value::Null),
        notify("exit", Value::Null),
    ]);
    assert_eq!(answer(&out, 2)["error"]["code"], -32602);
    // Input ending without exit is a failure too.
    let (report, _) = session(&[initialize(&s.uri(""), &[])]);
    assert!(report.failed);
    let _ = Path::new(&s.0);
}
