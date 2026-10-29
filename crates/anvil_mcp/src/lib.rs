//! Anvil's MCP server: every `anvil` command as a tool an agent calls directly.
//!
//! Each tool calls the same `anvil_ops` function as the CLI and returns its JSON, so the two
//! faces cannot disagree. Files are read fresh on every call, so edits apply at once. A
//! failing check (compile errors, a failing test, a panic, mechanical fixes that leave errors)
//! is a tool error, as it is a non-zero exit in the CLI: failures are loud here too.

use anvil_ops::Report;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Statements and loop iterations a `run` or `test` call may take before it is stopped, so a
/// program that loops forever cannot hang the session. Pass `max_steps` to change it.
pub const DEFAULT_MAX_STEPS: u64 = 100_000_000;

const INSTRUCTIONS: &str = "Anvil: a compiled language designed for agents. `spec` returns the whole \
language; read it once before writing Anvil. Write a .anv file, then `check` it: every error has a \
stable code, an exact range and fixes marked mechanical (safe) or changes_meaning (decide first). \
`fix` applies every mechanical fix and rechecks until none is left. `outline` lists a file's items \
and signatures without reading it all; `type_at` gives the type and declaration at a line and \
column; `refs` finds where a function, type, Type.field, Enum.Variant or unit is declared and used. \
`run` interprets main and returns its stdout; `test` runs the test blocks; `build` makes a native \
executable. `explain` describes an error code. Every CLI command is a tool with the same arguments \
(the CLI's `type-at` is `type_at`). Failing checks come back as tool errors.";

/// Tool argument structs reject unknown fields, so a misspelt argument is an error, not ignored.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// Stop after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// Executable to write (default: the file with .exe).
    pub out: Option<String>,
    /// Include each function's machine code in the result.
    pub asm: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypeAtArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// 1-based line.
    pub line: usize,
    /// 1-based column, counted in characters.
    pub col: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefsArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// A function, type, unit, `Type.field`, `Enum.Variant` or `Enum.Variant.field`.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainArgs {
    /// An error code such as E0301; leave it out to list every code.
    pub code: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Clone)]
pub struct AnvilServer {
    root: Arc<PathBuf>,
}

impl AnvilServer {
    /// Serves files under `root` (relative paths in tool arguments are resolved against it).
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = root.canonicalize()?;
        Ok(AnvilServer { root: Arc::new(root) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs an operation off the async thread and turns its report into a tool result.
    async fn op<F>(&self, op: F) -> Result<CallToolResult, ErrorData>
    where
        F: FnOnce(&Path) -> Report + Send + 'static,
    {
        let root = self.root.clone();
        let joined = tokio::task::spawn_blocking(move || op(&root)).await;
        Ok(match joined {
            Ok(report) => {
                let text = serde_json::to_string_pretty(&report.json)
                    .unwrap_or_else(|e| format!("{{\"error\": \"cannot encode the result: {e}\"}}"));
                let content = vec![ContentBlock::text(text)];
                if report.failed {
                    CallToolResult::error(content)
                } else {
                    CallToolResult::success(content)
                }
            }
            Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("error: the tool panicked: {e}"))]),
        })
    }
}

#[tool_router]
impl AnvilServer {
    #[tool(description = "Type-check a file. Errors come back with codes, ranges and typed fixes.")]
    async fn check(&self, Parameters(a): Parameters<FileArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::check(root, &a.file)).await
    }

    #[tool(description = "Interpret main (the reference semantics) and return its stdout and exit code. \
        A panic, or running out of steps, is a tool error.")]
    async fn run(&self, Parameters(a): Parameters<RunArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| {
            let mut out = Vec::new();
            let mut report = anvil_ops::run(root, &a.file, Some(a.max_steps.unwrap_or(DEFAULT_MAX_STEPS)), &mut out);
            report.json["stdout"] = serde_json::json!(String::from_utf8_lossy(&out));
            report
        })
        .await
    }

    #[tool(description = "Compile main to a native executable (Windows x64) and return its path, \
        and with asm the machine code of every function.")]
    async fn build(&self, Parameters(a): Parameters<BuildArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::build(root, &a.file, a.out.as_deref(), a.asm.unwrap_or(false))).await
    }

    #[tool(description = "Run every test block. Any failing test is a tool error; each result has \
        the panic message and the test's output.")]
    async fn test(&self, Parameters(a): Parameters<RunArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::test(root, &a.file, Some(a.max_steps.unwrap_or(DEFAULT_MAX_STEPS)))).await
    }

    #[tool(description = "Apply every mechanical fix to the file in place, recheck, and repeat until \
        none is left. Returns the fixes applied; errors that remain (they need a decision) make it a \
        tool error.")]
    async fn fix(&self, Parameters(a): Parameters<FileArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::fix(root, &a.file)).await
    }

    #[tool(description = "List the file's functions (with signatures, effects and contracts), structs, \
        enums, units and tests, with line numbers. Needs only a file that parses.")]
    async fn outline(&self, Parameters(a): Parameters<FileArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::outline(root, &a.file)).await
    }

    #[tool(description = "The type of the innermost expression at a line and column, and the \
        declaration of the name there (kind, signature or type, location). Works on files with \
        type errors.")]
    async fn type_at(&self, Parameters(a): Parameters<TypeAtArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::type_at(root, &a.file, a.line, a.col)).await
    }

    #[tool(description = "Where a function, type, unit, Type.field, Enum.Variant or Enum.Variant.field \
        is declared and every place it is used, with the source line of each.")]
    async fn refs(&self, Parameters(a): Parameters<RefsArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::refs(root, &a.file, &a.name)).await
    }

    #[tool(description = "Explain an error code, or list every code with its explanation.")]
    async fn explain(&self, Parameters(a): Parameters<ExplainArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |_| anvil_ops::explain(a.code.as_deref())).await
    }

    #[tool(description = "The whole Anvil language specification (Markdown).")]
    async fn spec(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(|_| anvil_ops::spec()).await
    }
}

#[tool_handler]
impl ServerHandler for AnvilServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("anvil", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

impl AnvilServer {
    /// Every tool this server serves (name, description, input schema), as `tools/list` sends them.
    pub fn tools() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }
}

/// Serves on stdin/stdout until the client disconnects.
pub async fn serve_stdio(server: AnvilServer) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::ServiceExt;
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
