//! `anvil docs`: a small static site from the sources that define the language, so it cannot
//! drift from them: the specification this binary was built with, every error code and lint
//! with its explanation, and the built-in functions and methods. Pages are plain HTML with one
//! stylesheet and no scripts, written the same way every time (no dates).

use crate::{Report, EXIT_FAILED};
use anvil_front::check::{BUILTIN_SIGNATURES, METHODS};
use anvil_front::diag;
use pulldown_cmark::{html, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::json;
use std::path::Path;

const STYLE: &str = "\
:root { --bg: #fdfdfb; --fg: #1d1f21; --muted: #5c6166; --line: #dcdcd6; --code: #f2f2ee; --link: #1f5fa8; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #16181b; --fg: #e3e3df; --muted: #9da1a6; --line: #2e3136; --code: #202328; --link: #7fb2f0; }
}
body { background: var(--bg); color: var(--fg); font: 16px/1.55 system-ui, sans-serif; margin: 0; }
nav { border-bottom: 1px solid var(--line); padding: 12px 16px; }
nav a { margin-right: 16px; }
main { max-width: 860px; margin: 0 auto; padding: 8px 16px 48px; }
a { color: var(--link); }
code, pre { background: var(--code); font: 14px/1.45 ui-monospace, monospace; }
code { padding: 1px 4px; border-radius: 3px; }
pre { padding: 12px; overflow-x: auto; border-radius: 4px; }
pre code { padding: 0; }
table { border-collapse: collapse; display: block; overflow-x: auto; }
th, td { border: 1px solid var(--line); padding: 4px 8px; text-align: left; vertical-align: top; }
.muted { color: var(--muted); }
";

/// A heading's anchor: lowercase letters and digits, words joined by `-`.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Markdown to HTML, every heading given an anchor; also the level-2 headings, for contents.
fn render(markdown: &str) -> (String, Vec<(String, String)>) {
    let events: Vec<Event> = Parser::new_ext(markdown, Options::ENABLE_TABLES).collect();
    let mut out_events = Vec::with_capacity(events.len());
    let mut contents = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading { level, classes, attrs, .. }) = &events[i] {
            let mut text = String::new();
            let mut j = i + 1;
            while !matches!(events[j], Event::End(TagEnd::Heading(_))) {
                if let Event::Text(t) | Event::Code(t) = &events[j] {
                    text.push_str(t);
                }
                j += 1;
            }
            let mut id = slug(&text);
            let mut n = 2;
            while seen.contains(&id) {
                id = format!("{}-{n}", slug(&text));
                n += 1;
            }
            seen.push(id.clone());
            if *level == HeadingLevel::H2 {
                contents.push((id.clone(), text));
            }
            out_events.push(Event::Start(Tag::Heading {
                level: *level,
                id: Some(CowStr::from(id)),
                classes: classes.clone(),
                attrs: attrs.clone(),
            }));
        } else {
            out_events.push(events[i].clone());
        }
        i += 1;
    }
    let mut body = String::new();
    html::push_html(&mut body, out_events.into_iter());
    (body, contents)
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n<link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n\
         <nav><a href=\"index.html\">Anvil</a><a href=\"spec.html\">Specification</a>\
         <a href=\"errors.html\">Errors</a><a href=\"builtins.html\">Built-ins</a>\
         <a href=\"std.html\">Standard library</a></nav>\n\
         <main>\n{body}</main>\n</body>\n</html>\n"
    )
}

/// The site's pages, by file name.
pub fn pages() -> Vec<(&'static str, String)> {
    let version = env!("CARGO_PKG_VERSION");
    let (spec, contents) = render(crate::SPEC);
    let toc: String =
        contents.iter().map(|(id, t)| format!("<li><a href=\"spec.html#{id}\">{}</a></li>\n", escape(t))).collect();
    let index = format!(
        "<h1>Anvil {version}</h1>\n<p>A compiled language built to be written by AI agents: one obvious way to \
         write each thing, stable error codes with typed fixes, units, effects and contracts checked by the \
         compiler, and two backends (an interpreter and native code) that must agree.</p>\n\
         <p class=\"muted\">Install the command with <code>cargo install anvil-lang</code>; \
         <code>anvil spec</code> prints the specification and <code>anvil explain CODE</code> an error.</p>\n\
         <h2>The specification</h2>\n<ul>\n{toc}</ul>\n"
    );
    let mut errors =
        String::from("<h1>Error codes and lints</h1>\n<p>What <code>anvil explain</code> says for each.</p>\n");
    for code in diag::ALL_CODES {
        let text = diag::explain(code).expect("every code has an explanation");
        errors.push_str(&format!("<h2 id=\"{code}\">{code}</h2>\n<p>{}</p>\n", escape(text)));
    }
    errors.push_str("<h2 id=\"lints\">Lints</h2>\n");
    for (name, text) in diag::LINTS {
        errors.push_str(&format!("<h3 id=\"{name}\">{name}</h3>\n<p>{}</p>\n", escape(text)));
    }
    let mut builtins = String::from(
        "<h1>Built-in functions and methods</h1>\n<table>\n<tr><th>Function</th><th>Signature</th></tr>\n",
    );
    for (name, sig) in BUILTIN_SIGNATURES {
        builtins.push_str(&format!("<tr><td><code>{name}</code></td><td><code>{}</code></td></tr>\n", escape(sig)));
    }
    builtins.push_str("</table>\n<h2>Methods</h2>\n<table>\n<tr><th>On</th><th>Method</th></tr>\n");
    for (on, _, sig) in METHODS {
        let on = match *on {
            "array" => "[T]",
            "map" => "Map[K, V]",
            "strings" => "[str]",
            other => other,
        };
        builtins.push_str(&format!(
            "<tr><td><code>{}</code></td><td><code>{}</code></td></tr>\n",
            escape(on),
            escape(sig)
        ));
    }
    builtins.push_str("</table>\n");
    let mut library = String::from(
        "<h1>The standard library</h1>\n<p>Modules written in Anvil and built into the compiler; \
         <code>anvil std MODULE</code> prints one's source.</p>\n",
    );
    for m in crate::std_modules(None).json["modules"].as_array().into_iter().flatten() {
        let name = m["module"].as_str().unwrap_or_default();
        library.push_str(&format!("<h2 id=\"{}\">{name}</h2>\n<pre><code>", slug(name)));
        library.push_str(&escape(&format!("import {name}.{{...}};\n\n")));
        for i in m["items"].as_array().into_iter().flatten() {
            let line = match i["signature"].as_str() {
                Some(sig) => sig.to_string(),
                None => format!("pub {} {}", i["kind"].as_str().unwrap_or(""), i["name"].as_str().unwrap_or("")),
            };
            library.push_str(&escape(&line));
            library.push('\n');
        }
        library.push_str("</code></pre>\n");
    }
    vec![
        ("index.html", page(&format!("Anvil {version}"), &index)),
        ("spec.html", page("The Anvil specification", &spec)),
        ("errors.html", page("Anvil error codes", &errors)),
        ("builtins.html", page("Anvil built-ins", &builtins)),
        ("std.html", page("The Anvil standard library", &library)),
        ("style.css", STYLE.to_string()),
    ]
}

/// Writes the site into `out` (default `out/docs`), relative to `root`.
pub fn docs(root: &Path, out: Option<&str>) -> Report {
    let dir = root.join(out.unwrap_or("out/docs"));
    let shown = dir.strip_prefix(root).unwrap_or(&dir).display().to_string().replace('\\', "/");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Report::error(format!("cannot create {shown}: {e}"));
    }
    let mut files = Vec::new();
    for (name, text) in pages() {
        if let Err(e) = std::fs::write(dir.join(name), text) {
            let r = Report::error(format!("cannot write {shown}/{name}: {e}"));
            return Report { exit: EXIT_FAILED, ..r };
        }
        files.push(format!("{shown}/{name}"));
    }
    let text = format!("wrote {} page(s) to {shown}; open {shown}/index.html\n", files.len());
    Report::ok(json!({ "ok": true, "dir": shown, "files": files }), text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_page_is_written_the_same_way_twice() {
        assert_eq!(super::pages(), super::pages());
    }

    #[test]
    fn spec_headings_get_anchors_once_each() {
        let (html, contents) = super::render("## Text\n\n### A\n\n## Text\n");
        assert!(html.contains("<h2 id=\"text\">") && html.contains("<h2 id=\"text-2\">"), "{html}");
        assert_eq!(contents.len(), 2);
    }
}
