//! `anvil lsp`: a language server on stdio for editors. Every answer comes from the same
//! operation as a command (`check`, `type-at`, `refs`, `complete`, `outline`, `fmt`, the fixes
//! of `check`), run on the editor's unsaved text, so an editor, the CLI and an agent over MCP
//! are told the same thing. Full-text sync; positions in UTF-32 when the client offers it
//! (Anvil's columns count characters), otherwise UTF-16. Requests it does not know are errors.

use crate::{check_in, complete_in, formatted_in, outline_in, refs_in, type_at_in, Files, Report};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// JSON-RPC error codes the server sends.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const REQUEST_FAILED: i64 = -32803;

/// Serves until the client sends `exit`. The report says whether `shutdown` came first, as
/// the protocol asks (exit code 0), or not (1).
pub fn lsp(input: &mut dyn BufRead, output: &mut dyn Write) -> Report {
    let mut s = Server {
        out: output,
        utf32: false,
        workspace: None,
        docs: HashMap::new(),
        diags: BTreeMap::new(),
        shut_down: false,
    };
    loop {
        let msg = match read_message(input) {
            Ok(Some(m)) => m,
            Ok(None) => return finished(false, "the client closed the input without `exit`"),
            Err(e) => return finished(false, &format!("bad message: {e}")),
        };
        let method = msg["method"].as_str().unwrap_or("").to_string();
        if method == "exit" {
            return finished(s.shut_down, if s.shut_down { "exit after shutdown" } else { "exit without shutdown" });
        }
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let answer = s.handle(&method, &params);
        if let Some(id) = id {
            let reply = match answer {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
                }
            };
            if s.send(&reply).is_err() {
                return finished(false, "cannot write to the client");
            }
        } else if let Err((_, message)) = answer {
            // A notification cannot be answered; say so where a person will see it.
            let _ = s.send(&json!({ "jsonrpc": "2.0", "method": "window/showMessage",
                "params": { "type": 1, "message": format!("anvil: {message}") } }));
        }
    }
}

fn finished(clean: bool, why: &str) -> Report {
    let json = json!({ "ok": clean, "stopped": why });
    if clean {
        Report::ok(json, String::new())
    } else {
        Report {
            json,
            stdout: String::new(),
            stderr: format!("anvil lsp: {why}\n"),
            exit: crate::EXIT_FAILED,
            failed: true,
        }
    }
}

/// One message: `Content-Length` headers, a blank line, then the JSON body. None at the end of input.
fn read_message(input: &mut dyn BufRead) -> Result<Option<Value>, String> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = Some(v.trim().parse::<usize>().map_err(|_| format!("bad Content-Length `{v}`"))?);
        }
    }
    let n = length.ok_or("a message without Content-Length")?;
    let mut body = vec![0; n];
    input.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map(Some).map_err(|e| e.to_string())
}

/// A JSON-RPC error: its code and message.
type Failure = (i64, String);

type Answer = Result<Value, Failure>;

struct Doc {
    text: String,
    path: PathBuf,
    /// The directory file names are relative to: the nearest one with anvil.toml, else the
    /// workspace if the file is in it, else the file's own directory.
    root: PathBuf,
    /// The file's name relative to `root`, with `/`.
    name: String,
}

struct Server<'o> {
    out: &'o mut dyn Write,
    utf32: bool,
    workspace: Option<PathBuf>,
    docs: HashMap<String, Doc>,
    /// Diagnostics by the file they are in, then by the open document whose check found them.
    diags: BTreeMap<String, BTreeMap<String, Vec<Value>>>,
    shut_down: bool,
}

impl Server<'_> {
    fn send(&mut self, msg: &Value) -> std::io::Result<()> {
        let body = msg.to_string();
        write!(self.out, "Content-Length: {}\r\n\r\n{body}", body.len())?;
        self.out.flush()
    }

    fn handle(&mut self, method: &str, p: &Value) -> Answer {
        match method {
            "initialize" => Ok(self.initialize(p)),
            "initialized" | "$/setTrace" | "$/cancelRequest" => Ok(Value::Null),
            "shutdown" => {
                self.shut_down = true;
                Ok(Value::Null)
            }
            "textDocument/didOpen" => {
                let d = &p["textDocument"];
                let uri = text(d, "uri")?;
                let doc = self.doc_for(&uri, text(d, "text")?)?;
                self.docs.insert(uri.clone(), doc);
                self.publish(&uri)
            }
            "textDocument/didChange" => {
                let uri = text(&p["textDocument"], "uri")?;
                let changes = p["contentChanges"].as_array().ok_or_else(|| bad("contentChanges"))?;
                let Some(last) = changes.last() else { return Ok(Value::Null) };
                if last.get("range").is_some() {
                    return Err((
                        INVALID_PARAMS,
                        "anvil syncs whole documents; a ranged change is not expected".into(),
                    ));
                }
                let new = text(last, "text")?;
                self.docs.get_mut(&uri).ok_or_else(|| not_open(&uri))?.text = new;
                self.publish(&uri)
            }
            "textDocument/didSave" => {
                let uri = text(&p["textDocument"], "uri")?;
                // Other open files may import this one.
                let open: Vec<String> = self.docs.keys().filter(|u| **u != uri).cloned().collect();
                for u in open {
                    self.publish(&u)?;
                }
                self.publish(&uri)
            }
            "textDocument/didClose" => {
                let uri = text(&p["textDocument"], "uri")?;
                self.docs.remove(&uri);
                self.set_diags(&uri, BTreeMap::new())?;
                Ok(Value::Null)
            }
            "textDocument/hover" => self.hover(p),
            "textDocument/definition" => self.definition(p),
            "textDocument/references" => self.references(p),
            "textDocument/completion" => self.completion(p),
            "textDocument/formatting" => self.formatting(p),
            "textDocument/documentSymbol" => self.symbols(p),
            "textDocument/codeAction" => self.code_actions(p),
            other => Err((METHOD_NOT_FOUND, format!("anvil lsp does not handle `{other}`"))),
        }
    }

    fn initialize(&mut self, p: &Value) -> Value {
        let offered = p["capabilities"]["general"]["positionEncodings"].as_array();
        self.utf32 = offered.is_some_and(|o| o.iter().any(|e| e == "utf-32"));
        let root = p["rootUri"].as_str().or_else(|| p["workspaceFolders"][0]["uri"].as_str());
        self.workspace = root.and_then(uri_path);
        json!({
            "capabilities": {
                "positionEncoding": if self.utf32 { "utf-32" } else { "utf-16" },
                "textDocumentSync": { "openClose": true, "change": 1, "save": { "includeText": false } },
                "hoverProvider": true,
                "definitionProvider": true,
                "referencesProvider": true,
                "completionProvider": { "triggerCharacters": ["."] },
                "documentFormattingProvider": true,
                "documentSymbolProvider": true,
                "codeActionProvider": { "codeActionKinds": ["quickfix"] },
            },
            "serverInfo": { "name": "anvil", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn doc_for(&self, uri: &str, text: String) -> Result<Doc, Failure> {
        let path = uri_path(uri).ok_or_else(|| (INVALID_PARAMS, format!("`{uri}` is not a file URI")))?;
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        let project = dir.ancestors().find(|d| d.join("anvil.toml").is_file()).map(Path::to_path_buf);
        let root = project.or_else(|| self.workspace.clone().filter(|w| path.starts_with(w))).unwrap_or(dir);
        let name = relative(&root, &path);
        Ok(Doc { text, path, root, name })
    }

    /// The files an operation on `uri` reads: the disk under its root, with every open document
    /// under that root read from the editor.
    fn files_for(&self, uri: &str) -> Result<(&Doc, HashMap<String, String>), Failure> {
        let doc = self.docs.get(uri).ok_or_else(|| not_open(uri))?;
        let open =
            self.docs.values().filter(|d| d.root == doc.root).map(|d| (d.name.clone(), d.text.clone())).collect();
        Ok((doc, open))
    }

    /// Checks `uri` and publishes what it found, in it and in the modules it imports.
    fn publish(&mut self, uri: &str) -> Answer {
        let (doc, open) = self.files_for(uri)?;
        let files = Files { root: &doc.root, open: Some(&open) };
        let report = check_in(files, &doc.name);
        let root = doc.root.clone();
        let mut found: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for d in report.json["diagnostics"].as_array().into_iter().flatten() {
            let path = root.join(d["file"].as_str().unwrap_or_default());
            let target = self.uri_for(&path);
            let lines = self.lines_of(&path);
            found.entry(target).or_default().push(self.diagnostic(d, &lines));
        }
        if report.failed && found.is_empty() {
            return Err((REQUEST_FAILED, report.stderr));
        }
        self.set_diags(uri, found)?;
        Ok(Value::Null)
    }

    /// Replaces what document `from` found, and republishes every file whose list changed.
    fn set_diags(&mut self, from: &str, found: BTreeMap<String, Vec<Value>>) -> Result<(), Failure> {
        // The document itself always gets a list, empty when it is clean.
        let mut touched: BTreeSet<String> = found.keys().cloned().collect();
        touched.insert(from.to_string());
        for (file, by_doc) in self.diags.iter_mut() {
            if by_doc.remove(from).is_some() {
                touched.insert(file.clone());
            }
        }
        for (file, list) in found {
            self.diags.entry(file).or_default().insert(from.to_string(), list);
        }
        for file in touched {
            let mut all: Vec<Value> =
                self.diags.get(&file).into_iter().flat_map(|m| m.values().flatten().cloned()).collect();
            all.dedup();
            let msg = json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": { "uri": file, "diagnostics": all } });
            self.send(&msg).map_err(|e| (REQUEST_FAILED, e.to_string()))?;
        }
        Ok(())
    }

    fn diagnostic(&self, d: &Value, lines: &[String]) -> Value {
        let mut message = d["message"].as_str().unwrap_or_default().to_string();
        for n in d["notes"].as_array().into_iter().flatten() {
            message.push_str(&format!("\nnote: {}", n["message"].as_str().unwrap_or_default()));
        }
        json!({
            "range": self.range(&d["range"], lines),
            "severity": 1,
            "code": d["code"],
            "source": "anvil",
            "message": message,
        })
    }

    /// The URI of a file: the client's own for an open document, so both name it alike.
    fn uri_for(&self, path: &Path) -> String {
        let same = |d: &&Doc| d.path.components().eq(path.components());
        match self.docs.iter().find(|(_, d)| same(d)) {
            Some((uri, _)) => uri.clone(),
            None => path_uri(path),
        }
    }

    /// The lines of a file, for converting columns: an open document's text, else the disk's.
    fn lines_of(&self, path: &Path) -> Vec<String> {
        let text = match self.docs.values().find(|d| d.path.components().eq(path.components())) {
            Some(d) => d.text.clone(),
            None => std::fs::read_to_string(path).unwrap_or_default(),
        };
        text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect()
    }

    /// An Anvil range (`{start: {line, col}, end: {line, col}}`, 1-based) as an LSP range.
    fn range(&self, r: &Value, lines: &[String]) -> Value {
        let at = |p: &Value| {
            self.position(p["line"].as_u64().unwrap_or(1) as usize, p["col"].as_u64().unwrap_or(1) as usize, lines)
        };
        json!({ "start": at(&r["start"]), "end": at(&r["end"]) })
    }

    /// A 1-based line and character column as an LSP position.
    fn position(&self, line: usize, col: usize, lines: &[String]) -> Value {
        let chars = col.saturating_sub(1);
        let character = if self.utf32 {
            chars
        } else {
            let text = lines.get(line.saturating_sub(1)).map(String::as_str).unwrap_or("");
            text.chars().take(chars).map(char::len_utf16).sum()
        };
        json!({ "line": line.saturating_sub(1), "character": character })
    }

    /// The document and 1-based line and column of a request's `textDocument` and `position`.
    fn at(&self, p: &Value) -> Result<(String, usize, usize), Failure> {
        let uri = text(&p["textDocument"], "uri")?;
        let doc = self.docs.get(&uri).ok_or_else(|| not_open(&uri))?;
        let line = p["position"]["line"].as_u64().ok_or_else(|| bad("position.line"))? as usize;
        let character = p["position"]["character"].as_u64().ok_or_else(|| bad("position.character"))? as usize;
        let col = if self.utf32 {
            character
        } else {
            let text = doc.text.split('\n').nth(line).unwrap_or("");
            let mut units = 0;
            text.chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= character
                })
                .count()
        };
        Ok((uri, line + 1, col + 1))
    }

    /// Runs an operation on the document a request names, at its position.
    fn query(
        &self,
        p: &Value,
        op: impl FnOnce(Files, &str, usize, usize) -> Report,
    ) -> Result<(Report, PathBuf), Failure> {
        let (uri, line, col) = self.at(p)?;
        let (doc, open) = self.files_for(&uri)?;
        let report = op(Files { root: &doc.root, open: Some(&open) }, &doc.name, line, col);
        Ok((report, doc.root.clone()))
    }

    /// A declaration's place (`file`, `defined_at`) as an LSP location.
    fn location(&self, root: &Path, file: &str, at: &Value) -> Value {
        let path = root.join(file);
        let uri = self.uri_for(&path);
        let lines = self.lines_of(&path);
        let line = at["line"].as_u64().unwrap_or(1) as usize;
        let start = self.position(line, at["col"].as_u64().unwrap_or(1) as usize, &lines);
        let end = self.position(line, at["end_col"].as_u64().unwrap_or(1) as usize, &lines);
        json!({ "uri": uri, "range": { "start": start, "end": end } })
    }

    fn hover(&self, p: &Value) -> Answer {
        let (r, _) = self.query(p, type_at_in)?;
        if r.failed {
            return Ok(Value::Null);
        }
        let mut parts = Vec::new();
        if let (Some(text), Some(ty)) = (r.json["text"].as_str(), r.json["type"].as_str()) {
            parts.push(format!("```anvil\n{}: {ty}\n```", one_line(text)));
        }
        let d = &r.json["declaration"];
        if d.is_object() {
            let at = &d["defined_at"];
            parts.push(format!(
                "{} `{}` declared at {}:{}:{}\n\n```anvil\n{}\n```",
                d["kind"].as_str().unwrap_or_default(),
                d["name"].as_str().unwrap_or_default(),
                d["file"].as_str().unwrap_or_default(),
                at["line"],
                at["col"],
                d["detail"].as_str().unwrap_or_default()
            ));
        }
        Ok(json!({ "contents": { "kind": "markdown", "value": parts.join("\n\n") } }))
    }

    fn definition(&self, p: &Value) -> Answer {
        let (r, root) = self.query(p, type_at_in)?;
        let d = &r.json["declaration"];
        if r.failed || !d.is_object() {
            return Ok(Value::Null);
        }
        Ok(self.location(&root, d["file"].as_str().unwrap_or_default(), &d["defined_at"]))
    }

    fn references(&self, p: &Value) -> Answer {
        let (r, root) = self.query(p, |files, file, line, col| refs_in(files, file, &format!("{line}:{col}")))?;
        if r.failed {
            return Ok(json!([]));
        }
        let (uri, _, _) = self.at(p)?;
        let name = &self.docs[&uri].name;
        let mut out = Vec::new();
        for d in r.json["declarations"].as_array().into_iter().flatten() {
            if p["context"]["includeDeclaration"] == true {
                out.push(self.location(&root, d["file"].as_str().unwrap_or_default(), &d["defined_at"]));
            }
            for u in d["uses"].as_array().into_iter().flatten() {
                out.push(self.location(&root, name, u));
            }
        }
        Ok(json!(out))
    }

    fn completion(&self, p: &Value) -> Answer {
        let (r, _) = self.query(p, complete_in)?;
        if r.failed {
            return Err((REQUEST_FAILED, failure(&r)));
        }
        let items: Vec<Value> = r.json["items"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, item)| {
                json!({
                    "label": item["label"],
                    "kind": completion_kind(item["kind"].as_str().unwrap_or_default()),
                    "detail": item["detail"],
                    // Keep the order complete gives: names in scope first, keywords last.
                    "sortText": format!("{i:05}"),
                })
            })
            .collect();
        Ok(json!({ "isIncomplete": false, "items": items }))
    }

    fn formatting(&self, p: &Value) -> Answer {
        let uri = text(&p["textDocument"], "uri")?;
        let (doc, open) = self.files_for(&uri)?;
        let r = formatted_in(Files { root: &doc.root, open: Some(&open) }, &doc.name);
        if r.failed {
            return Err((REQUEST_FAILED, failure(&r)));
        }
        if r.json["changed"] != true {
            return Ok(json!([]));
        }
        // One edit replacing the whole document.
        let lines: Vec<&str> = doc.text.split('\n').collect();
        let last = lines.last().copied().unwrap_or("");
        let end_char = if self.utf32 { last.chars().count() } else { last.encode_utf16().count() };
        let range = json!({ "start": { "line": 0, "character": 0 }, "end": { "line": lines.len() - 1, "character": end_char } });
        Ok(json!([{ "range": range, "newText": r.json["text"] }]))
    }

    fn symbols(&self, p: &Value) -> Answer {
        let uri = text(&p["textDocument"], "uri")?;
        let (doc, open) = self.files_for(&uri)?;
        let r = outline_in(Files { root: &doc.root, open: Some(&open) }, &doc.name);
        if r.failed {
            return Err((REQUEST_FAILED, failure(&r)));
        }
        let lines: Vec<&str> = doc.text.split('\n').collect();
        let symbols: Vec<Value> = r.json["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| {
                let line = item["line"].as_u64().unwrap_or(1) as usize - 1;
                let width = lines.get(line).map_or(0, |l| {
                    let l = l.trim_end_matches('\r');
                    if self.utf32 { l.chars().count() } else { l.encode_utf16().count() }
                });
                let range = json!({ "start": { "line": line, "character": 0 }, "end": { "line": line, "character": width } });
                let kind = item["kind"].as_str().unwrap_or_default();
                let name = match kind {
                    "test" => format!("test \"{}\"", item["name"].as_str().unwrap_or_default()),
                    "import" => format!("import {}", item["module"].as_str().unwrap_or_default()),
                    _ => item["name"].as_str().unwrap_or_default().to_string(),
                };
                let detail = item["signature"].as_str().or_else(|| item["text"].as_str()).unwrap_or_default();
                json!({ "name": name, "detail": detail, "kind": symbol_kind(kind), "range": range, "selectionRange": range })
            })
            .collect();
        Ok(json!(symbols))
    }

    /// The fixes of the errors in the requested range, mechanical ones preferred.
    fn code_actions(&self, p: &Value) -> Answer {
        let uri = text(&p["textDocument"], "uri")?;
        let (doc, open) = self.files_for(&uri)?;
        let r = check_in(Files { root: &doc.root, open: Some(&open) }, &doc.name);
        let lines: Vec<String> = doc.text.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
        let want = (&p["range"]["start"], &p["range"]["end"]);
        let line_of = |v: &Value| v["line"].as_u64().unwrap_or(0);
        let mut actions = Vec::new();
        for d in r.json["diagnostics"].as_array().into_iter().flatten() {
            if d["file"] != doc.name.as_str() {
                continue;
            }
            let (first, last) = (line_of(&d["range"]["start"]) - 1, line_of(&d["range"]["end"]) - 1);
            if last < line_of(want.0) || first > line_of(want.1) {
                continue;
            }
            for f in d["fixes"].as_array().into_iter().flatten() {
                let edits: Vec<Value> = f["edits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| json!({ "range": self.range(&e["range"], &lines), "newText": e["text"] }))
                    .collect();
                let mechanical = f["kind"] == "mechanical";
                let mut changes = serde_json::Map::new();
                changes.insert(uri.clone(), json!(edits));
                actions.push(json!({
                    "title": format!("{} ({})", f["description"].as_str().unwrap_or_default(), d["code"].as_str().unwrap_or_default()),
                    "kind": "quickfix",
                    "isPreferred": mechanical,
                    "diagnostics": [self.diagnostic(d, &lines)],
                    "edit": { "changes": changes },
                }));
            }
        }
        Ok(json!(actions))
    }
}

fn text(v: &Value, key: &str) -> Result<String, Failure> {
    v[key].as_str().map(str::to_string).ok_or_else(|| bad(key))
}

fn bad(what: &str) -> Failure {
    (INVALID_PARAMS, format!("missing or invalid `{what}`"))
}

fn not_open(uri: &str) -> Failure {
    (INVALID_PARAMS, format!("`{uri}` is not open; send textDocument/didOpen first"))
}

/// What a failed report says, for an error response.
fn failure(r: &Report) -> String {
    let s =
        if r.stderr.is_empty() { r.json["error"].as_str().unwrap_or("failed").to_string() } else { r.stderr.clone() };
    s.trim_end().to_string()
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn completion_kind(kind: &str) -> u32 {
    match kind {
        "local" | "param" => 6,
        "field" => 5,
        "method" => 2,
        "variant" => 20,
        "fn" | "builtin" => 3,
        "struct" => 22,
        "enum" => 13,
        "type" => 7,
        "unit" => 11,
        _ => 14,
    }
}

fn symbol_kind(kind: &str) -> u32 {
    match kind {
        "import" => 2,
        "struct" => 23,
        "enum" => 10,
        "unit" => 14,
        _ => 12,
    }
}

/// `file:///C:/a%20b/c.anv` or `file:///home/a/c.anv` as a path.
fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::new();
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex: String = [it.next()? as char, it.next()? as char].iter().collect();
            bytes.push(u8::from_str_radix(&hex, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    // `/C:/x` on Windows; drive letters in upper case, so one file has one name.
    let path = match path.strip_prefix('/') {
        Some(p) if p.as_bytes().get(1) == Some(&b':') => format!("{}{}", p[..1].to_uppercase(), &p[1..]),
        _ => path,
    };
    Some(PathBuf::from(path))
}

fn path_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `path` relative to `root`, with `/`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_paths_round_trip() {
        let p = uri_path("file:///c%3A/Users/a%20b/x.anv").unwrap();
        assert_eq!(p, PathBuf::from("C:/Users/a b/x.anv"));
        assert_eq!(path_uri(&p), "file:///C:/Users/a%20b/x.anv");
        assert_eq!(uri_path("file:///home/a/x.anv").unwrap(), PathBuf::from("/home/a/x.anv"));
        assert_eq!(path_uri(Path::new("/home/a/x.anv")), "file:///home/a/x.anv");
        assert!(uri_path("untitled:1").is_none());
    }
}
