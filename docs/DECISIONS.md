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

## M6 (v0.7)

- **Map keys are i64 or str, in ascending order.** Sorted order, not insertion order: it never
  depends on history, so equal maps print, iterate and compare alike. Natively a map is the
  array runtime holding sorted (key, value) elements, found by binary search.
- **`[k: v]` literals and `[:]`**, after arrays' `[x]` and `[]`; no new keyword or braces.
- **No silent cases:** a repeated literal key, `remove` of a missing key, a slice out of range
  or through a character, and `split` at `""` all panic. `get` returns an Option instead.
- **`==` on every type**, structural, floats as IEEE values. Every type's parts have `==`, so
  the roadmap's "whose parts all have it" holds for all of them; there is no opt-out.
- **Slices copy** (value semantics, like everything else); both bounds are required, so there
  is one way to write a range.
- **`set` names its arguments** (`key:`, `value:`), like any call with two parameters.

## M7 (v0.8)

- **Imports are relative to the importing file**, not to the program's root, so every file can
  be checked, tested and run alone, and a module means the same thing from every importer. There
  is no `..`: modules form a tree below the files that use them.
- **`import path.{names};`, names only.** No wildcards and no module-qualified use sites
  (`geometry.area`): an agent reading a file sees every outside name it uses at the top.
- **Unused imports are errors with a mechanical fix**, like unused variables.
- **One program, many files:** the checker runs once per module, dependencies first, each with
  its own name tables; structs, enums and functions share one id space, so the backends are
  unchanged apart from unique native symbols. Option/Result instantiations and native type
  layouts are keyed by type, not by name, since two modules may each have a `Shape`.
- **Coverage is project-wide:** a module's function counts as tested when any project file's
  tests or golden-checked main run it.

## M8 (v0.9)

- **Monomorphised in the checker:** each set of type arguments gets its own function or type
  in the IR, so the interpreter and native backend did not change and generic code costs
  nothing at runtime.
- **Bodies are checked once, abstractly**, with each parameter as `Ty::Param`, so errors show
  up in the generic itself, once, whether or not anything calls it. Instances are checked again
  quietly; anything they find is reported in the body with a note naming the instance.
- **Three built-in constraints, no user traits:** `ordered`, `text` and `key` cover what the
  operators and built-ins need; `==` needs none, since every type has it. "Use-site-visible"
  means the constraint is in the signature, and a call that breaks it fails at the call.
- **Type arguments are never written at a use**, only in types. They are read from what is
  passed, which is why every parameter must appear in a parameter or field type (E0700); one
  way to call a generic.
- **Generics count as tested** when any of their instances runs; one that nothing instantiates
  is untested.
- **Endless instances are an error** (E0704), found by counting how many instances deep a chain
  of instances goes.

## M9 (v0.10)

- **Property tests are `test ... for all inputs`**, a kind of test rather than a new item, so
  they run, count for coverage and appear in `verify` like any test.
- **Seeded by the test's name**: the same cases every run, so a property test never flakes and
  a failure is reproducible from its seed. Inputs grow with the case number and failures shrink
  greedily, one input at a time, so the report is a small counterexample.
- **Generated floats are finite.** nan would make most equality properties fail for reasons
  that are IEEE's, not the code's; a test that cares about nan writes the case out.
- **`test --native` covers plain tests; property tests stay in the interpreter**, the
  reference. Making the same values natively would mean a second generator; fuzzing already
  checks the backends agree.
- **The fuzzer generates typed programs that end**: helpers call only earlier helpers and
  loops count to small constants. Panics are kept (half the programs end in one) because both
  backends must give the same panic. 100 programs agreed when it was added.

## M10 (v0.11)

- **Native code is for the host.** Cranelift and the C runtime are built for the machine the
  compiler runs on; there is no `--target` yet, since a cross build would need that target's
  linker and C library anyway. The release workflow builds a compiler on each platform instead.
- **The C runtime owns `main`.** The generated code exports `anvil_program`; `main` saves argv
  (for test executables) and, outside Windows, runs the program on a thread with 64 MiB of stack,
  the size `/STACK` gives on Windows, so deep recursion behaves alike everywhere.
- **Position-independent code on Linux and macOS** (PIE is the default there); Windows keeps a
  fixed base. `cc` (or `$CC`) links, with `-lm -lpthread`.
- **Only what ran is ticked.** Windows x64 and Linux x86-64 (in WSL) passed the whole suite,
  every golden on both backends, fuzzing and the release script. ARM64 Linux and macOS have their
  code paths and workflow jobs but have not run, so that roadmap item stays open.

## M11 (v0.12)

- **The language server is a face, like the CLI and MCP.** `anvil lsp` turns each request into
  a command (`check`, `type-at`, `refs`, `complete`, `outline`, `fmt`) run on the editor's text
  through `Files`, an overlay of open documents on the disk, so it cannot know anything the
  commands do not. It lives in `anvil_ops` and needs only `serde_json`.
- **`complete` is a command too**, so agents get the same candidates over MCP. It inserts a
  placeholder name (and a `;`) at the cursor when that is what it takes to parse; the item
  holding the cursor must otherwise parse, and an error says so.
- **Items with syntax errors are left out, not the whole file.** Canonical layout puts every
  item at column 1, so a broken item is blanked (offsets kept) and the rest checked; answers
  count the items left out in `errors_in_file`.
- **Facts follow imports.** Declarations carry their file, uses point to a file and span, and the
  items of imported modules are kept, so definition, hover and `refs` reach into modules. Locals
  carry their scope, so completion offers only those visible at the cursor.
- **An expression statement that does nothing (E0392) is still checked**, for its facts only: its
  other errors are dropped, so it reports what it did before.
- **UTF-32 positions when offered**, since Anvil columns count characters; UTF-16 otherwise.
  Unknown requests are errors (MethodNotFound), as a silent null would hide a missing feature.

## M12 (v0.13)

- **Three effects, declared like io.** `clock` and `random` are effects because they make a
  function's result depend on more than its arguments; a caller should see that in the
  signature. The order in `uses` is fixed (io, clock, random) so the formatter has one layout and
  fixes insert an effect where it belongs.
- **Allocation is a promise, not an effect.** Almost every function that touches text or arrays
  allocates, so `uses alloc` would sit on most signatures and say little. `noalloc` marks the
  few that must not, and the checker proves it from a fixed list of operations that may
  allocate (the runtime calls that do). A noalloc function calls only noalloc functions, so the
  promise is visible at every call, as effects are, rather than inferred across files.
- **print never allocates**, natively too: numbers and bools are written by the runtime without
  building a str, so a noalloc function can still report.
- **Tests and goldens read a virtual clock** (1 ms per read) and executables do when
  `ANVIL_CLOCK=virtual`, so everything checked stays deterministic; `anvil run` reads the real
  one. Monotonic only: no dates, which would need time zones.
- **Randomness is one seeded sequence, SplitMix64 from seed 0**, specified in SPEC and
  implemented twice (`ir::Random`, the C runtime), so goldens with random numbers mean the same
  thing on every platform and in every version. Seeding from the clock is the program's choice.
- **C is reached through a shim in the interpreter.** The interpreter loads a library built from
  the C files and a generated `anvil_ffi_call(func, args, result)`, so it needs no assembly or
  libffi; only i64 and f64 cross, as their bits. The shim is compiled without optimisation so a
  call to libm stays a call to the same function an executable links.
- **Extern declarations are trusted, so they say everything**: effects and noalloc as for any
  function. `uses io` is refused, because C writes past the output Anvil captures (and would
  corrupt the MCP and LSP streams). C files are read through the same loader as modules, so
  editors' unsaved text and `check` see them, and compiled in their own directory so no path
  reaches the object and executables stay byte-identical.
- **`outline` reports `effects`, `noalloc` and `extern`** in place of `uses_io`.

## M13 (v0.14)

- **Input before publishing.** A program could not read anything, so the language was not yet
  general-purpose; the user chose this milestone ahead of publishing, which became M14.
- **Arguments and stdin are io; files are a new effect, `fs`.** io is the program's terminal,
  both ways. Files outlive the run and are shared with everything else, so a signature says
  when a function touches them. The order is io, fs, clock, random.
- **Checked runs read the disk and write to memory.** Tests, goldens, traces and profiles read
  files under the root they run in (fixtures in the repository work) and keep writes for the
  run, so they are deterministic and never change the disk. A write in memory fails where the
  disk would fail it (a missing directory, a directory), so a passing test means a passing run.
  Executables do the same with `ANVIL_FS=memory`, which golden and test runs set.
- **Errors are an enum, not text.** `FileError` has five variants the program matches
  exhaustively; operating-system messages differ by platform and would break goldens. The
  checks that decide the variant (is it a directory, is it UTF-8, is the path empty) come
  first and are made the same way in both backends.
- **A golden's input is a file a person writes**, `golden/<file>.in`, in the `.out` style (a
  header, `---`, bytes), so goldens stay one approval of observed output. Over MCP there is no
  terminal, so `run` reads stdin only from `input`.
- **Program arguments come after `--`**, so they never clash with Anvil's options; over MCP
  they are a list. A positional ending in `*` in `COMMANDS` is this list, so the parity test
  covers it.
- **`write_file` names its arguments.** Two str parameters are exactly the swap named
  arguments prevent; built-ins with one parameter stay positional.
- **Parsing is specified, then delegated.** `parse_i64` and `parse_f64` check one strict
  syntax themselves (no `+`, `_`, spaces, `inf` or hex) and only then call Rust's parser or
  `strtod`, both correctly rounded; a sweep of 20,000 random floats reads back exactly on
  both backends. Not-a-number and infinities are not text a program parses.
- **Windows executables start at `wmain`**, so arguments arrive as UTF-16 and reach the
  program as UTF-8; paths go back to UTF-16 for the file calls.
