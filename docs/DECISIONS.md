# Decisions

Short records of choices that are not obvious from the code. Newest last.

## M0

- **The interpreter is the reference.** The native backend is correct when it matches the
  interpreter on stdout, panic message and exit code. Failure messages are built once, in the
  checker, and stored in the IR, so the backends cannot word them differently.
- **Structs are flattened into scalars in codegen.** Values, parameters and returns are lists
  of I64/F64/I8 values; there is no memory layout to get wrong. Cranelift's
  `enable_multi_ret_implicit_sret` handles wide returns. Revisit when arrays arrive.
- **The C runtime is compiled at build time and embedded** in the compiler, so `anvil` is one
  binary. Static CRT. `/Brepro` makes executables byte-identical across builds.
- **stdout is binary mode** in native programs, so `\n` is never rewritten to `\r\n` and both
  backends print identical bytes.
- **Float text is Anvil's own format** (shortest round-trip digits, always a `.`), implemented
  twice (Rust `ir::format_f64`, C `anvil_format_f64`) and compared on hundreds of values by
  `tests/programs/floats_sweep.anv`.
- **Units are erased at runtime.** Printing a float with a unit appends the unit text, which the
  compiler knows from the static type.
- **Named arguments for two or more parameters**, positional for one: swapped arguments are the
  most common bug a type checker cannot catch when both have the same type.
- **Fix kinds.** A fix is `mechanical` only when it cannot change what the author meant (adding
  `;`, naming arguments in their existing order, the unit stated by a declared type). Anything
  that guesses intent is `changes_meaning`. Where a guess could compile and be wrong, there is
  no fix at all (unterminated strings).
- **Unused variables and unneeded `var` are errors**, not warnings: Anvil has no warnings.
- **The parser stops at the first syntax error**; the checker reports every error, using an
  error type so one mistake does not cascade.
- **Recursion limits differ**: the interpreter panics past 10,000 nested calls; native code has
  a 64 MiB stack. Recorded in SPEC.md as the one known difference.
- **The `anvil` crate name is taken on crates.io** (and Foundry ships a tool called Anvil). The
  language keeps the name; the package will publish as `anvil-lang`.

## M1 (v0.2)

- **Arrays are values.** Assignment copies; storage is shared until written (copy-on-write).
  No aliasing means a change is only ever visible through the name it was made with.
- **Native memory is reference counted**, with every local released when its function returns
  and a store releasing the value it replaces. `ANVIL_CHECK_LEAKS=1` makes a native program
  fail at exit if any object is still alive; the parity tests always set it.
- **Enums flatten to a tag plus every variant's fields**, unused ones zero, so they stay in
  scalars like structs. Array elements are 8-byte slots, one per scalar.
- **No catch-all match arm.** A wildcard would hide new variants from every existing match.
- **One `print` argument.** Interpolation is the one way to build text; the old multi-argument
  form is an error with a mechanical fix that rewrites it, which migrated every test program.
- **`?` only at the top of let, assignment and return**, so an early return never leaves
  half-evaluated temporaries behind in native code.
- **Strings may appear inside `{...}`**, after the first version forbade it and the author of
  the tests tripped over the rule three times.

## M2 (v0.3)

- **The agent surface follows the Nautilus engine's**: one ops layer (`anvil_ops`) that both
  the CLI and the MCP server call, a parity test that walks `COMMANDS` and the tool list both
  ways, failing checks as tool errors, and argument structs that reject unknown fields.
- **The checker keeps facts** (declarations, uses pointing at declarations, the type of every
  expression) even when a file has errors, so `type-at` and `refs` work on code mid-edit.
- **`fix` applies only mechanical fixes**, repeatedly, and fails if errors remain: anything
  that needs a decision is left to the author.
- **Steps, not wall-clock time, limit runs**, so the limit is deterministic. MCP calls default
  to 100,000,000 steps so a loop cannot hang a session; the CLI has no default limit.
- **Not taken from the engine (yet):** the SQLite project record and review sheets. A language
  repo's ROADMAP and DECISIONS are small enough, and there are no pictures to sign off.

## M3 (v0.4)

- **The rest of the engine's agent framework**: goldens, the SQLite project record (same rules:
  a regression opens a new finding, unknown statuses and kinds are errors, a newer schema is
  refused) and a review sheet. The review sheet is Markdown, since Anvil has no pictures.
- **A golden stores exit code and panic as well as stdout**, so a program that should panic is
  checked like any other; the old `// exit:` and `// panic:` headers are gone.
- **The repository dogfoods it.** Moving the corpus was checked mechanically: all 32 old
  expected outputs equalled the new goldens byte for byte before the old files were deleted.
- **Findings are keyed by file, check and message**, with the position kept apart (`at`), so
  editing other lines of a file does not resolve and reopen its findings.
- **The record holds wall-clock times**; it is history about the project, never program output,
  so determinism is unaffected.

## M4 (v0.5)

- **What the engine's sim timeline and telemetry became.** `trace` is the timeline (calls,
  arguments, returns, prints) and `profile` the telemetry, both from the interpreter. They count
  steps, never time, so the numbers are the same on every machine and can go in a test.
- **Values in a trace are written as Anvil source** (`2.0[m]`, `Shape.Circle(1.5)`, `Some(3)`),
  cut after 8 elements, 80 string bytes or 200 characters, always saying how much was cut.
- **A panic is what `trace` observed, not a failure of `trace`**, so the command succeeds and the
  end of the timeline says how the run ended.
- **`untested` is the first lint**, and lints use the engine's escape hatch: a per-function
  `allow untested "reason"` with a required reason, never a looser global setting. `main` is
  left to its golden, which `verify` already reports when it is missing.
- **`watch` is the one command with no tool.** It runs until stopped; an MCP client calls
  `verify` after its own edits. The parity test reads the exception and its reason from
  `CLI_ONLY_COMMANDS`, as the engine's `parity.rs` does.

## M5 (v0.6)

- **The layout is part of the language.** A file that is not in canonical layout fails `check`
  (E0110), and so `run`, `test` and `verify`, with the formatted file as its mechanical fix.
  There are no warnings, so it could only be an error or nothing; agents call `fix` anyway.
  It is reported only for a file with no other error, so real errors come first.
- **Printed from the syntax tree, with literals copied from the source**, so `1_000` and
  `2.5e-3` keep their spelling and parentheses stay as written. The test checks every program
  in the repository keeps its tokens and comments and that formatting twice changes nothing.
- **A comment the layout cannot place is an error (E0111), never dropped.** Comments live
  between entries (items, statements, fields, variants, arms) or at the end of a line.
- **One width rule (100 columns), applied narrowly:** a one-statement match arm stays on one
  line if it fits, and a long statement splits its array, call or struct literal one element
  per line. Anything else stays on one line however long; a general line breaker can come later.
- **Parser recovery restarts at item keywords only.** They never appear inside an item, so the
  restart point is never a guess; within an item only the first error is reported.
