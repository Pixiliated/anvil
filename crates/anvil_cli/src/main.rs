//! The `anvil` command. Every command is an `anvil_ops` operation; this file only parses
//! arguments against `anvil_ops::COMMANDS` and prints the report. Quiet by default (failures and
//! one summary line), `--json` for tools. Exit codes: 0 success, 1 compile errors or failures,
//! 2 usage errors; `run` passes through the program's own exit code, 101 on panic.

use anvil_ops::{Command, ProgramInput, Report, COMMANDS};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

fn usage() -> String {
    let mut s = String::from("usage: anvil <command> [args] [--json]\n\n");
    for c in COMMANDS {
        let mut line = format!("  {}", c.name);
        for p in c.positionals.iter().filter(|p| !p.ends_with('*')) {
            match p.strip_suffix('?') {
                Some(opt) => line.push_str(&format!(" [{}]", opt.to_uppercase())),
                None => line.push_str(&format!(" <{}>", p.to_uppercase())),
            }
        }
        for (name, value, _) in c.options {
            if value.is_empty() {
                line.push_str(&format!(" [--{name}]"));
            } else {
                line.push_str(&format!(" [--{name} {value}]"));
            }
        }
        if let Some(rest) = c.positionals.iter().find_map(|p| p.strip_suffix('*')) {
            line.push_str(&format!(" [-- {}...]", rest.to_uppercase()));
        }
        s.push_str(&format!("{line:<48} {}\n", c.summary));
    }
    s.push_str("\n  -o is short for --out. --json prints the result as JSON (for run, on stderr).");
    s.push_str("\n  anvil --version prints the version and the target native builds produce.");
    s
}

struct Parsed {
    command: &'static Command,
    positionals: Vec<String>,
    /// Everything after `--`: the program's own arguments.
    rest: Vec<String>,
    options: HashMap<&'static str, Option<String>>,
    json: bool,
}

fn parse_args() -> Result<Parsed, String> {
    let mut it = std::env::args().skip(1).peekable();
    let first = it.next().ok_or("missing command")?;
    // Two-word commands (`golden check`, `record query`) take the next word too.
    let two = it.peek().map(|second| format!("{first} {second}"));
    let command = match two.as_deref().and_then(|n| COMMANDS.iter().find(|c| c.name == n)) {
        Some(c) => {
            it.next();
            c
        }
        None => match COMMANDS.iter().find(|c| c.name == first) {
            Some(c) => c,
            None if COMMANDS.iter().any(|c| c.name.starts_with(&format!("{first} "))) => {
                let subs: Vec<&str> =
                    COMMANDS.iter().filter_map(|c| c.name.strip_prefix(&format!("{first} "))).collect();
                return Err(format!("`{first}` needs one of: {}", subs.join(", ")));
            }
            None => return Err(format!("unknown command `{first}`")),
        },
    };
    let name = command.name;
    let mut p = Parsed { command, positionals: Vec::new(), rest: Vec::new(), options: HashMap::new(), json: false };
    let takes_rest = command.positionals.iter().any(|p| p.ends_with('*'));
    while let Some(arg) = it.next() {
        let long = match arg.as_str() {
            "--" if takes_rest => {
                p.rest = it.by_ref().collect();
                break;
            }
            "--" => return Err(format!("`{name}` takes no program arguments after `--`")),
            "--json" => {
                p.json = true;
                continue;
            }
            "-o" => "out",
            a if a.starts_with("--") => &a[2..],
            a if a.starts_with('-') && a.len() > 1 => return Err(format!("unknown option `{a}`")),
            _ => {
                p.positionals.push(arg);
                continue;
            }
        };
        let Some((opt, value, _)) = command.options.iter().find(|(n, _, _)| *n == long) else {
            let valid: Vec<String> = command.options.iter().map(|(n, _, _)| format!("--{n}")).collect();
            return Err(format!(
                "`{name}` has no option `--{long}`{}",
                if valid.is_empty() { String::new() } else { format!("; it takes {}", valid.join(", ")) }
            ));
        };
        let v = if value.is_empty() { None } else { Some(it.next().ok_or(format!("--{opt} needs a value"))?) };
        p.options.insert(opt, v);
    }
    let named: Vec<&str> = command.positionals.iter().copied().filter(|p| !p.ends_with('*')).collect();
    let required = named.iter().filter(|p| !p.ends_with('?')).count();
    if p.positionals.len() < required || p.positionals.len() > named.len() {
        let names: Vec<&str> = named.iter().map(|p| p.trim_end_matches('?')).collect();
        let after = if takes_rest { "; the program's own arguments go after `--`" } else { "" };
        return Err(format!(
            "`{name}` takes {} ({}), found {} argument(s){after}",
            named.len(),
            names.join(", "),
            p.positionals.len()
        ));
    }
    Ok(p)
}

fn number<T: std::str::FromStr>(what: &str, text: &str) -> Result<T, String> {
    text.parse().map_err(|_| format!("{what} must be a whole number, found `{text}`"))
}

fn dispatch(p: &Parsed) -> Result<Report, String> {
    let root = Path::new(".");
    let pos = |i: usize| p.positionals.get(i).map(String::as_str);
    let file = pos(0).unwrap_or("");
    let steps = match p.options.get("max-steps") {
        Some(Some(v)) => Some(number::<u64>("--max-steps", v)?),
        _ => None,
    };
    let native = p.options.contains_key("native");
    let text = |name: &str| p.options.get(name).and_then(|v| v.as_deref());
    let count = |name: &str| -> Result<Option<u64>, String> {
        text(name).map(|v| number::<u64>(&format!("--{name}"), v)).transpose()
    };
    let input =
        |own_stdin: bool| ProgramInput { args: p.rest.clone(), file: text("input").map(String::from), own_stdin };
    Ok(match p.command.name {
        "check" => anvil_ops::check(root, file),
        "run" => anvil_ops::run(root, file, steps, &input(true), (&mut std::io::stdout(), &mut std::io::stderr())),
        "build" => {
            let out = p.options.get("out").and_then(|v| v.as_deref());
            anvil_ops::build(root, file, out, p.options.contains_key("asm"))
        }
        "test" => anvil_ops::test(root, file, steps, native, count("cases")?, count("seed")?),
        "fmt" => anvil_ops::fmt(root, pos(0), p.options.contains_key("check")),
        "fix" => anvil_ops::fix(root, file),
        "outline" => anvil_ops::outline(root, file),
        "type-at" => anvil_ops::type_at(root, file, number("line", pos(1).unwrap())?, number("col", pos(2).unwrap())?),
        "refs" => anvil_ops::refs(root, file, pos(1).unwrap()),
        "complete" => {
            anvil_ops::complete(root, file, number("line", pos(1).unwrap())?, number("col", pos(2).unwrap())?)
        }
        "lsp" => anvil_ops::lsp(&mut std::io::stdin().lock(), &mut std::io::stdout()),
        "explain" => anvil_ops::explain(pos(0)),
        "spec" => anvil_ops::spec(),
        "docs" => anvil_ops::docs(root, text("out")),
        "std" => anvil_ops::std_modules(pos(0)),
        "trace" => anvil_ops::trace(root, file, text("test"), text("only"), count("limit")?, steps, &input(false)),
        "profile" => anvil_ops::profile(root, file, text("test"), steps, &input(false)),
        "fuzz" => anvil_ops::fuzz(root, count("seed")?, count("count")?),
        "list" => anvil_ops::list(root),
        "verify" => anvil_ops::verify(root, native, steps),
        "review" => anvil_ops::review(root, native, steps),
        "mutate" => anvil_ops::mutate(root, file, text("only"), steps),
        "api" => anvil_ops::api(root, pos(0)),
        "api approve" => anvil_ops::api_approve(root, file),
        "golden check" => anvil_ops::golden_check(root, pos(0), native, steps),
        "golden approve" => anvil_ops::golden_approve(root, file, steps),
        "record query" => anvil_ops::record_query(root, pos(0), p.options.contains_key("open")),
        "record decide" => anvil_ops::record_decide(root, file, pos(1)),
        "record status" => anvil_ops::record_status(root, file, pos(1).unwrap()),
        "record import-roadmap" => anvil_ops::record_import_roadmap(root, pos(0)),
        "record export" => anvil_ops::record_export(root),
        "watch" => {
            let json = p.json;
            anvil_ops::watch(root, native, steps, count("runs")?, &mut |r: &Report, more: bool| {
                if json {
                    println!("{}", r.json);
                } else {
                    print!("{}", r.stdout);
                    eprint!("{}", r.stderr);
                    if more {
                        eprintln!("watching for changes (Ctrl+C to stop)");
                    }
                }
                let _ = std::io::stdout().flush();
            })
        }
        other => unreachable!("command `{other}` is in COMMANDS but not dispatched"),
    })
}

fn main() -> ExitCode {
    if std::env::args().skip(1).collect::<Vec<_>>() == ["--version"] {
        println!("anvil {}", anvil_ops::version());
        return ExitCode::SUCCESS;
    }
    let parsed = match parse_args() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", usage());
            return ExitCode::from(anvil_ops::EXIT_USAGE);
        }
    };
    let report = match dispatch(&parsed) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}\n\n{}", usage());
            return ExitCode::from(anvil_ops::EXIT_USAGE);
        }
    };
    if parsed.json {
        // `run` keeps stdout for the program's own output.
        if parsed.command.name == "run" {
            eprintln!("{}", report.json);
        } else {
            println!("{}", report.json);
        }
    } else {
        print!("{}", report.stdout);
        eprint!("{}", report.stderr);
    }
    let _ = std::io::stdout().flush();
    ExitCode::from(report.exit)
}
