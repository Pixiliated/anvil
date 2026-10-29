//! `anvil-mcp [--root <dir>]`: Anvil's MCP server on stdio. Stdout carries the protocol, so
//! every message for humans goes to stderr.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: anvil-mcp [--root <dir>]   (MCP over stdio; file paths resolve against the root, default: the current directory)";

fn main() -> ExitCode {
    let mut root = PathBuf::from(".");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => match args.next() {
                Some(p) => root = PathBuf::from(p),
                None => {
                    eprintln!("error: --root needs a directory\n{USAGE}");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                eprintln!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("error: unknown argument `{other}`\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let server = match anvil_mcp::AnvilServer::new(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot use {} as the root: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    eprintln!("anvil-mcp: serving {} on stdio", server.root().display());
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: starting the async runtime: {e}");
            return ExitCode::from(2);
        }
    };
    match runtime.block_on(anvil_mcp::serve_stdio(server)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
