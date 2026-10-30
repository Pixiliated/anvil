//! The formatter over every program in the repository: it keeps every token (it may add a
//! trailing comma to a list it splits) and every comment, and formatting its output changes
//! nothing.

use anvil_front::lexer::{self, LexPart, Tok};
use anvil_front::span::SourceFile;
use std::path::Path;

fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    for dir in ["examples", "tests/programs", "tests/errors"] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "anv") {
                out.push((
                    format!("{dir}/{}", p.file_name().unwrap().to_string_lossy()),
                    std::fs::read_to_string(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    assert!(out.len() > 60, "found only {} programs", out.len());
    out
}

/// The tokens of `text` without positions, leaving out commas that end a list.
fn tokens(text: &str) -> Vec<String> {
    let toks: Vec<Tok> = lexer::lex(text).unwrap().into_iter().map(|t| t.tok).collect();
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let closes = matches!(toks.get(i + 1), Some(Tok::RParen | Tok::RBracket | Tok::RBrace));
        if !(*t == Tok::Comma && closes) {
            out.push(describe(t));
        }
    }
    out
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Interp(parts) => {
            let parts: Vec<String> = parts
                .iter()
                .map(|p| match p {
                    LexPart::Lit(s) => format!("{s:?}"),
                    LexPart::Code(code) => code.iter().map(|c| describe(&c.tok)).collect::<Vec<_>>().join(" "),
                })
                .collect();
            format!("Interp[{}]", parts.join(" | "))
        }
        t => format!("{t:?}"),
    }
}

fn comment_texts(text: &str) -> Vec<String> {
    lexer::comments(text).iter().map(|s| text[s.start..s.end].trim_end().to_string()).collect()
}

#[test]
fn formatting_keeps_tokens_and_comments_and_settles() {
    let mut formatted_any = 0;
    for (name, text) in sources() {
        let src = SourceFile::new(name.clone(), text.clone());
        let Ok(out) = anvil_front::fmt::format(&src) else { continue };
        formatted_any += 1;
        assert_eq!(tokens(&out), tokens(&text), "{name}: tokens changed:\n{out}");
        assert_eq!(comment_texts(&out), comment_texts(&text), "{name}: comments changed:\n{out}");
        let again = anvil_front::fmt::format(&SourceFile::new(name.clone(), out.clone())).unwrap();
        assert_eq!(again, out, "{name}: formatting the output changed it");
    }
    assert!(formatted_any > 60);
}

#[test]
fn comments_keep_their_places() {
    let text = "// file header\n\nunit m;\nunit s;\n// speed\nstruct Body {\n    // how fast\n    v: f64[m / s], // per second\n}\nfn f(b: Body) -> f64[m/s] {\n\n    // first\n    let v: f64[m/s] = b.v;    // read\n\n\n    return v;\n    // after the last statement\n} // end of f\n\n\n// trailing\n";
    let out = anvil_front::fmt::format(&SourceFile::new("t.anv", text.to_string())).unwrap();
    assert_eq!(
        out,
        "// file header\n\nunit m;\nunit s;\n\n// speed\nstruct Body {\n    // how fast\n    v: f64[m/s], // per second\n}\n\nfn f(b: Body) -> f64[m/s] {\n    // first\n    let v: f64[m/s] = b.v; // read\n\n    return v;\n    // after the last statement\n} // end of f\n\n// trailing\n"
    );
}
