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
`fix` applies every mechanical fix and rechecks until none is left. A program may span files: \
`import lib.shapes.{area, Shape};` reads lib/shapes.anv next to the importing file, and only pub \
items can be imported; errors in a module carry that module's file. Generics: \
`fn max[T: ordered](a: T, b: T) -> T`; type arguments are inferred, never written at calls. A file must be in canonical \
layout (E0110 otherwise; its fix is the formatted file); `fmt` formats a file or the whole project, \
and keeps comments. `outline` lists a file's items \
and signatures without reading it all; `type_at` gives the type and declaration at a line and \
column; `refs` finds where a function, type, Type.field, Enum.Variant or unit (or, given LINE:COL, \
whatever is there) is declared and used; `complete` lists what can be written at a position. \
`run` interprets main and returns its stdout; `test` runs the test blocks; `build` makes a native \
executable. `trace` shows a run as a timeline of calls with their arguments, returns and prints \
(narrow it with `only`); `profile` counts calls and steps per function (steps, not time, so the \
numbers are the same everywhere). `explain` describes an error code or a lint. Projects (a directory with anvil.toml) add `list`, \
`verify` (check, test and golden-check every file; findings go into the project record), \
`golden_check` and `golden_approve` (approved output per program; approve only after reading the \
output), `review` (writes out/review.md for a person to sign off) and the record: `record_query` \
(start here in a project: milestones, decisions, findings with history, runs), `record_decide`, \
`record_status`, `record_import_roadmap`, `record_export`. `verify` also reports functions that no \
test and no golden-checked main runs (the `untested` lint; `allow untested \"why\"` before a fn \
switches it off). Every CLI command except `watch` and `lsp` is a tool with the same arguments (the CLI's \
`type-at` is `type_at`, `golden check` is `golden_check`). Failing checks come back as tool errors.";

/// Tool argument structs reject unknown fields, so a misspelt argument is an error, not ignored.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocsArgs {
    /// Directory for the pages, relative to the server's root (default out/docs).
    pub out: Option<String>,
}

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
    /// A file whose bytes are the program's stdin (default: empty).
    pub input: Option<String>,
    /// The program's command-line arguments, what args() returns.
    pub args: Option<Vec<String>>,
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
    /// A function, type, unit, `Type.field`, `Enum.Variant` or `Enum.Variant.field`; or
    /// `LINE:COL` for whatever is declared or used there, parameters and locals included.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainArgs {
    /// An error code such as E0301, or a lint such as untested; leave it out to list them all.
    pub code: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifyArgs {
    /// Stop each run after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
    /// Also build and run each program natively (slower; checks the native backend and leaks).
    pub native: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FuzzArgs {
    /// Seed of the first program (default 1); a seed always makes the same program.
    pub seed: Option<u64>,
    /// Programs to try (default 10); each is built natively, so each takes a second or two.
    pub count: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// Stop each test (or property case) after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
    /// Also run the plain tests as a native executable; its results must equal the interpreter's.
    pub native: Option<bool>,
    /// Cases per property test (`for all`); default 100.
    pub cases: Option<u64>,
    /// Seed for the property tests' generated inputs; default 0, so runs repeat.
    pub seed: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FmtArgs {
    /// Path of the .anv file, relative to the server's root or absolute; leave it out to format
    /// every project file.
    pub file: Option<String>,
    /// Change nothing; report the files that are not in canonical layout as a tool error.
    pub check: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TraceArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// Trace this test (by name) instead of main.
    pub test: Option<String>,
    /// Keep the calls and returns of these functions only, comma-separated; prints are always kept.
    pub only: Option<String>,
    /// Keep at most this many events (default 1000); the rest are counted as dropped.
    pub limit: Option<u64>,
    /// Stop the run after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
    /// A file whose bytes are main's stdin (default: empty).
    pub input: Option<String>,
    /// main's command-line arguments, what args() returns.
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileArgs {
    /// Path of the .anv file, relative to the server's root or absolute.
    pub file: String,
    /// Profile this test (by name) instead of main.
    pub test: Option<String>,
    /// Stop the run after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
    /// A file whose bytes are main's stdin (default: empty).
    pub input: Option<String>,
    /// main's command-line arguments, what args() returns.
    pub args: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoldenCheckArgs {
    /// One project file to check; leave it out to check them all.
    pub file: Option<String>,
    /// Stop each run after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
    /// Also build and run each program natively.
    pub native: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GoldenApproveArgs {
    /// The project file whose current output becomes its golden.
    pub file: String,
    /// Stop the run after this many statements and loop iterations (default 100000000).
    pub max_steps: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordQueryArgs {
    /// milestones, decisions, findings or runs; leave it out for a summary.
    pub kind: Option<String>,
    /// Only open findings.
    pub open: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideArgs {
    /// One line naming the decision.
    pub title: String,
    /// Why, and what it rules out.
    pub body: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    /// Milestone id, such as M3 or M3.2.
    pub milestone: String,
    /// open, active, done or dropped.
    pub status: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportArgs {
    /// The roadmap file; leave it out for docs/ROADMAP.md in the project.
    pub file: Option<String>,
}

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
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let input = program_input(a.args, a.input);
            let mut report = anvil_ops::run(root, &a.file, Some(steps(a.max_steps)), &input, (&mut out, &mut err));
            report.json["stdout"] = serde_json::json!(String::from_utf8_lossy(&out));
            report.json["stderr"] = serde_json::json!(String::from_utf8_lossy(&err));
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
    async fn test(&self, Parameters(a): Parameters<TestArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| {
            anvil_ops::test(root, &a.file, Some(steps(a.max_steps)), a.native.unwrap_or(false), a.cases, a.seed)
        })
        .await
    }

    #[tool(description = "Generate random well-typed programs (from a seed, so each is reproducible) and \
        run each in the interpreter and natively with the leak check. Any difference in output, panic \
        or exit code is a backend bug; failing programs are saved in out/fuzz/<seed>.anv.")]
    async fn fuzz(&self, Parameters(a): Parameters<FuzzArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::fuzz(root, a.seed, a.count)).await
    }

    #[tool(description = "Put a file (or, without file, every project file) in canonical layout, \
        keeping its comments. With check, change nothing and make any unformatted file a tool error.")]
    async fn fmt(&self, Parameters(a): Parameters<FmtArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::fmt(root, a.file.as_deref(), a.check.unwrap_or(false))).await
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

    #[tool(description = "What can be written at a line and column (the column the next \
        character would take): after `.`, the fields and methods of the receiver's type or the \
        variants of the enum named; elsewhere every local, parameter and item in scope there, \
        the built-ins, the types and the keywords. Only candidates starting with the name \
        already typed before the column.")]
    async fn complete(&self, Parameters(a): Parameters<TypeAtArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::complete(root, &a.file, a.line, a.col)).await
    }

    #[tool(description = "Run main (or one test) in the interpreter and return a timeline: each call with \
        its arguments written as Anvil values, each return value and each printed line, with the step \
        and call depth. A panic or the step limit ends the timeline; it is reported, not a tool error.")]
    async fn trace(&self, Parameters(a): Parameters<TraceArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| {
            let (test, only) = (a.test.as_deref(), a.only.as_deref());
            let input = program_input(a.args, a.input);
            anvil_ops::trace(root, &a.file, test, only, a.limit, Some(steps(a.max_steps)), &input)
        })
        .await
    }

    #[tool(description = "Run main (or one test) in the interpreter and count, per function, its calls, \
        its steps with callees and its own steps; also the functions that never ran. Steps are \
        statements and loop iterations, so the numbers are deterministic.")]
    async fn profile(&self, Parameters(a): Parameters<ProfileArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| {
            let input = program_input(a.args, a.input);
            anvil_ops::profile(root, &a.file, a.test.as_deref(), Some(steps(a.max_steps)), &input)
        })
        .await
    }

    #[tool(description = "Explain an error code or a lint, or list every code and lint with its explanation.")]
    async fn explain(&self, Parameters(a): Parameters<ExplainArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |_| anvil_ops::explain(a.code.as_deref())).await
    }

    #[tool(description = "The whole Anvil language specification (Markdown).")]
    async fn spec(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(|_| anvil_ops::spec()).await
    }

    #[tool(
        description = "Write a static site under the server's root: the specification, every error         code and lint with its explanation, and the built-in functions and methods. Returns the files."
    )]
    async fn docs(&self, Parameters(a): Parameters<DocsArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::docs(root, a.out.as_deref())).await
    }

    #[tool(description = "The project's files (from anvil.toml): functions, tests, whether each has a main \
        and an approved golden.")]
    async fn list(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(anvil_ops::list).await
    }

    #[tool(description = "Check, test and golden-check every project file, store what fails as findings in \
        the project record (new ones open, fixed ones resolve) and return the open ones. Any open finding \
        makes it a tool error.")]
    async fn verify(&self, Parameters(a): Parameters<VerifyArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::verify(root, a.native.unwrap_or(false), Some(steps(a.max_steps)))).await
    }

    #[tool(description = "Run verify, then write out/review.md: summary, open findings, every file's \
        signatures and test and golden status, recent decisions, and boxes for a person to sign off. \
        Returns the Markdown.")]
    async fn review(&self, Parameters(a): Parameters<VerifyArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::review(root, a.native.unwrap_or(false), Some(steps(a.max_steps)))).await
    }

    #[tool(description = "Compare each program's output, exit code and panic with its approved golden \
        (golden/<file>.out); with native, also as an executable that must free every object. Any \
        difference is a tool error naming the first differing line.")]
    async fn golden_check(&self, Parameters(a): Parameters<GoldenCheckArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| {
            anvil_ops::golden_check(root, a.file.as_deref(), a.native.unwrap_or(false), Some(steps(a.max_steps)))
        })
        .await
    }

    #[tool(description = "Store a program's current output as its golden and return that output and what \
        changed. Read the output before approving: an approved golden is what every later check trusts.")]
    async fn golden_approve(&self, Parameters(a): Parameters<GoldenApproveArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::golden_approve(root, &a.file, Some(steps(a.max_steps)))).await
    }

    #[tool(description = "Read the project record: milestones, decisions, findings (with first seen, last \
        seen and resolved times) or verify runs; no kind gives a summary with the current milestone.")]
    async fn record_query(&self, Parameters(a): Parameters<RecordQueryArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::record_query(root, a.kind.as_deref(), a.open.unwrap_or(false))).await
    }

    #[tool(description = "Record a decision in the project record, dated today.")]
    async fn record_decide(&self, Parameters(a): Parameters<DecideArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::record_decide(root, &a.title, a.body.as_deref())).await
    }

    #[tool(description = "Set a milestone's status in the project record: open, active, done or dropped.")]
    async fn record_status(&self, Parameters(a): Parameters<StatusArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::record_status(root, &a.milestone, &a.status)).await
    }

    #[tool(description = "Load milestones into the project record from a roadmap: `## M1: Title` headings \
        and `- [ ]` / `- [x]` checkboxes. The first unfinished milestone becomes active.")]
    async fn record_import_roadmap(&self, Parameters(a): Parameters<ImportArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(move |root| anvil_ops::record_import_roadmap(root, a.file.as_deref())).await
    }

    #[tool(description = "The whole project record as Markdown, for people.")]
    async fn record_export(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        self.op(anvil_ops::record_export).await
    }
}

fn steps(given: Option<u64>) -> u64 {
    given.unwrap_or(DEFAULT_MAX_STEPS)
}

/// Over MCP there is no terminal: without `input`, the program's stdin is empty.
fn program_input(args: Option<Vec<String>>, input: Option<String>) -> anvil_ops::ProgramInput {
    anvil_ops::ProgramInput { args: args.unwrap_or_default(), file: input, own_stdin: false }
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
