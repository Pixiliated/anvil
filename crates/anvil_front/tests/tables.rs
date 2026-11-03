//! The tables completion shows must say what the language does.

use anvil_front::check::{BUILTINS, BUILTIN_SIGNATURES, CONSTRUCTORS, METHODS};
use anvil_front::lexer::{lex, Tok, KEYWORDS};
use anvil_front::span::SourceFile;

#[test]
fn every_listed_method_checks_with_its_signature() {
    // One program calling every method, with the result bound to the type the table gives.
    let mut body = String::from(
        "    let s: str = \"a,b\";\n    var xs: [i64] = [1, 2];\n    var m: Map[str, i64] = [\"k\": 1];\n    let n: i64 = 7;\n    let bs: bytes = bytes_of([1, 2]);\n",
    );
    for (n, (on, name, sig)) in METHODS.iter().enumerate() {
        let receiver = match *on {
            "str" => "s",
            "array" => "xs",
            "map" => "m",
            "i64" => "n",
            "bytes" => "bs",
            other => panic!("unknown receiver kind {other}"),
        };
        let fill = |t: &str| t.replace('T', "i64").replace('K', "str").replace('V', "i64");
        let params = &sig[sig.find('(').unwrap() + 1..sig.find(')').unwrap()];
        let args: Vec<String> = params
            .split(", ")
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (pname, ty) = p.split_once(": ").unwrap();
                let value = if fill(ty) == "str" { "\"k\"".to_string() } else { "2".to_string() };
                if *name == "set" {
                    format!("{pname}: {value}")
                } else {
                    value
                }
            })
            .collect();
        let call = format!("{receiver}.{name}({})", args.join(", "));
        match sig.split_once(" -> ") {
            Some((_, ret)) => body.push_str(&format!("    let _r{n}: {} = {call};\n", fill(ret))),
            None => body.push_str(&format!("    {call};\n")),
        }
    }
    let text = format!("fn main() {{\n{body}}}\n");
    let src = SourceFile::new("methods.anv", text.clone());
    let a = anvil_front::analyze(&src);
    let errors: Vec<String> = a.diags.iter().map(|d| format!("{} {}", d.code, d.message)).collect();
    assert!(errors.is_empty(), "{text}\n{errors:#?}");
}

#[test]
fn builtin_signatures_follow_the_builtins() {
    let names: Vec<&str> = BUILTIN_SIGNATURES.iter().map(|(n, _)| *n).collect();
    let want: Vec<&str> = BUILTINS.iter().chain(CONSTRUCTORS).copied().collect();
    assert_eq!(names, want);
}

#[test]
fn every_keyword_is_one() {
    for k in KEYWORDS {
        let toks = lex(k).unwrap();
        assert!(!matches!(toks[0].tok, Tok::Ident(_)), "`{k}` lexes as a name");
    }
}
