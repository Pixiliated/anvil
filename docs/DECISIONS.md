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

## M14 (v0.15)

- **Function values before interfaces.** Passing behaviour (a comparator, a callback) was the
  largest gap; interfaces can come later, and need no change to these types.
- **Captures are copies, made when the literal is evaluated.** It is the rule for every other
  value: nothing is shared, so a closure cannot change behind the code that made it, and the
  body cannot assign a captured name (E0410) because only the copy would change.
- **Function types have no parameter names, so calls through a value are positional.** Names
  in the type would make `fn(a: i64)` and `fn(x: i64)` different types, and passing `max` where
  `fn(i64, i64) -> i64` is expected would need a rename. Named arguments stay the rule for
  declared functions.
- **A function that does less fits a type that allows more**, at the point a value is passed,
  bound or returned (effects contained, noalloc kept); nowhere else is there subtyping, and the
  value is the same either way.
- **A function type after `->` is in parentheses** (the formatter writes them), so `uses` after
  it always belongs to the declaration; inside a type, a comma continues a `uses` list only with
  an effect name.
- **Literals are lifted into functions** whose first parameters are the captured values: the
  interpreter calls them with the captures prepended, native code passes the closure object
  (refcounted, the captured values after a header the runtime frees through) and the function
  loads its captures from it. Declared functions get a small thunk; closures that capture
  nothing are static objects, so they never allocate.
- **Function literals are not linted as untested**: they belong to the function they are
  written in, which is.

## M15 (v0.16)

- **Methods are declared next to their type** (`fn Circle.area(self)`), in the type's module, so
  a value's methods are found by its type and need no import, and two types can each have
  `area`. Built-in and imported types take none: a function does the same job.
- **Methods are functions with a receiver**, no new semantics: `self` is a value like any
  parameter, so "mutating" methods return the new value. Both backends needed no change.
- **Interfaces are constraints only.** Generics are already monomorphised, so `T: Shape` adds no
  runtime cost, and a list of different shapes is an enum, which keeps match exhaustive. Values
  of interface type (vtables) can come later without changing this syntax.
- **Meeting an interface is structural**: a type that has the methods meets it, and E0701 says
  which method is missing or differs. An `impl` declaration would repeat the signatures.
- **`self` is a keyword and may go unused**: an interface can ask for a method whose
  implementation needs nothing from its value.
- **Privacy applies where a call is written.** An instance of a generic from another module
  calls the type's methods through its interfaces, private or not: the type's module chose to
  pass the value there.

## M16 (v0.17)

- **No `^` operator.** `^` is already a power in units, and `x ^ 2` reads as a square to many,
  so it would compute the wrong thing silently; exclusive or is `a.xor(b)`, and `^` in an
  expression is E0393 with that as its fix. `& | << >> ~` mean the same in every major language
  and stay operators, with C and Rust precedence (a mix-up with comparisons is a type error).
- **Shifts are checked**: an amount outside 0..=63 panics, since machines disagree on what it
  does. Bits shifted out are dropped: a shift is not arithmetic, so it does not overflow.
- **Wrapping arithmetic is spelled out** (`wrapping_mul`): plain `*` still panics on overflow,
  so a hash's intent shows at each operation.
- **Hex and binary literals are bit patterns**, all 64 bits, because masks and hash constants
  are written that way (`0xcbf29ce484222325` is negative as an i64).
- **`bytes` is its own type, the same object as `str` natively** (a str is bytes that are
  UTF-8), so `to_bytes` costs nothing and `to_str` only checks. An array of i64 would take eight
  times the memory; `bytes_of` and `to_array` convert.
- **`6.xor(3)` is a method call**: a digit, `.` and a letter cannot be a float.

## M17 (v0.18)

- **A standard library before publishing.** Everyday programs kept rewriting the same basics
  (joining, sorting, reporting an error); these are the ones most programs need, specified to
  give the same text on both backends.
- **Case is ASCII.** Unicode case mapping depends on tables that change between versions;
  ASCII letters change and every other character stays, the same everywhere.
- **`fixed` rounds the exact binary value, ties to even**, as both Rust's formatter and C's
  printf do (checked on both backends, Windows and Linux), so `0.125.fixed(2)` is `0.12`.
- **`sorted` is stable and refuses NaN**: one order for every input, including `0.0` and
  `-0.0`. The runtime has its own merge sort, as qsort is not stable.
- **stderr is part of a golden** (`stderr:` lines in the header), and of a test's result, so
  error messages are checked like output. A panic stays apart from it: natively it is the last
  line of stderr when the exit code is 101.
- **`exit` ends a function's path**, so `fn f() -> i64 { ...; exit(1); }` needs no dead
  `return`; in a test it is a failure, since a test ends by returning.
- **Checked runs see only the environment they are given** (`env:` lines; none for tests):
  native goldens run with a cleared environment, and `ANVIL_` names read as None, so the
  runtime's own settings cannot leak into output.
- **`FileError` gained `NotDirectory`**, at the end, so existing variant numbers stay; matches
  that list every variant need the new arm.

## M18 (v0.19)

- **Packages are named `anvil-lang` and `anvil-lang-*`** (`anvil` and `anvil_cli` are taken on
  crates.io); each library keeps its Rust name (`[lib] name = "anvil_front"`), so no code
  changed. The CLI package is `anvil-lang`, so `cargo install anvil-lang` gives `anvil`.
- **Packages carry copies** of the spec, the README and the licenses, because a package holds
  only its crate's directory; a test fails when a copy drifts, which is louder than a build
  script that reads outside the package.
- **Checked from the packages themselves**: `cargo package --workspace` builds each crate from
  its packaged files, and an `anvil` built from the unpacked packages alone built a native
  executable. Publishing waits for the owner: the repository URL, the commit email in the
  history, and the upload.
- **The docs site is generated by `anvil docs`** from the spec compiled into the binary and the
  error explanations in the checker, so it cannot drift from them; pulldown-cmark renders the
  Markdown, and output is the same bytes every time.

## M19 (v0.20)

- **The standard library is written in Anvil.** Generics, function values, methods and
  interfaces make it possible, it needs no backend work, and both backends, the fuzzer-checked
  code paths and every tool treat it like any module. Built-ins stay for what Anvil cannot
  express or needs fast (text search, sorting primitives, the system).
- **`std.` always means the built-in modules**, from any file, so a program reads the same
  everywhere; the sources live in anvil_front and ship with its package, and the repository's
  project includes them, so verify requires a test for every function.
- **The functions passed in are pure**: Anvil has no effect variables, so `map` cannot take a
  function that prints. A loop does that job.
- **`Set` is a struct over `Map[T, bool]`**, so its members are the map's key types; an empty one
  is written with its type, since nothing else tells its element type.
- **`\r` became an escape**, because text that handles "\r\n" line endings has to write it.

## M20 (v0.21)

- **Parallelism is one pure map**, not threads, tasks or locks: pure functions cannot observe
  each other, so `parallel_map` keeps every program deterministic and the interpreter (a plain
  loop) stays the reference. Effects are refused (E0416) rather than ordered.
- **Workers share nothing.** Each gets deep copies of the function and of its contiguous share of
  the array, so reference counts stay plain (non-atomic) and fast; only the count of live
  objects is atomic. Copying costs O(input), small next to the work worth spreading.
- **The first panic wins**: a panicking worker records its message and parks, workers skip
  elements after the smallest panicking index, and the program panics with that one, so the
  panic is the loop's. Workers still busy past it are ended by the exit.
- **`ANVIL_THREADS`** pins the worker count (for measuring and for tests); it is read only by
  native programs, and never changes a result.

## M21 (v0.22)

- **Branch coverage is a lint, not a report**: an agent acts on findings, and a percentage
  invites "good enough". Like `untested`, it is switched off per function with a reason.
- **Branches are blocks** (then, else, arm, loop body), marked by a statement the checker puts
  first in each; the interpreter counts it without a step, so step counts, profiles and goldens
  do not change, and native code drops it.
- **An empty block is not a branch.** Exhaustive `match` makes arms like `None => {}` exist where
  no value is missing; reaching nothing tests nothing, so requiring a test would only add allows.
- **Functions that never run are left to `untested`** and main to its golden, so one gap is one
  finding. `&&`, `||` and `?` are not branches yet: their paths have no block to point at.
- On its first run the lint found real gaps: four standard-library branches no test took
  (`max_by_key` replacing its best, `ceil`/`round` of huge values, `round` of a fraction over a
  half, `words` ending without a space), all now tested.

## M22 (v0.23)

- **Mutants are made in the source, not the IR**: a change is one span of text, so it is
  reported where it is written, goes through the same checker as real code (one that no longer
  type-checks is invalid, not a crash), and is formatted first so a left-out line is no layout
  error.
- **A mutant is run against everything that could notice it**: the file's tests and golden, and
  every project file whose program includes the file, as verify's coverage counts importers.
- **Step budgets come from the unchanged run** (ten times its steps), so a mutant that loops is
  killed in a fixed, deterministic number of steps, not by a timer.
- **Equivalent mutants are excused per function with a reason**, like the lints, because no tool
  can always tell them apart; the allow goes stale when nothing survives.
- **Not part of verify**: it runs the tests once per mutant (the repository's 70 files make about
  1,900 mutants, about two minutes). The corpus programs check the backends, not their own
  tests, so they are not held to it; the standard library is.
- Its first run on the standard library found tests that could not tell `sign(1)` from 0, the
  first of equal keys in `min_by_key`/`max_by_key`, or one-letter words; they have tests now.

## M23 (v0.24)

- **A contract is a specification verify can run**: with requires naming the inputs a
  function takes and ensures what it promises, generated arguments check it with no test
  written. It is a lint in verify (one verdict), using the property tests' generator and
  shrinker, seeded by the function's name, so it finds the same thing every run.
- **Requires are evaluated first, on their own**, so an input they exclude is told apart from
  one the function fails on; a callee's requires failing is the caller's finding, since its own
  requires let the input through.
- **Overflow counts as excluded, not broken.** Generated inputs include i64's extremes, so nearly
  every arithmetic function would otherwise be flagged, and contracts would become something to
  avoid. Overflow is the language refusing the input range, loudly, at run time either way.
- **Too slow is a finding** (over a million steps on one call): an endless loop looks the same,
  and a requires can bound the inputs. Only pure functions are checked: generated arguments
  cannot stand in for files, stdin or the clock.
- It found `pow` looping `exp` times (`pow(1, i64::MAX)` never finished; it squares now, in at
  most 63 rounds) and the projectile example accepting a negative mass and a zero time step.

## M24 (v0.25)

- **A doc is the comment above the item, with no new syntax**: the comments people already
  wrote become docs, and the formatter keeps them where they are. Blank lines end a doc, so a
  file's header comment is not its first item's.
- **Docs travel with the facts.** The checker records each declaration's doc, so hover,
  completion and type-at show it from any file that imports the item, and an agent reads
  `outline` or `std` instead of the source.
- **Examples are tests, not text**: `// example: EXPR` is parsed from the comment's own span and
  added to the file's tests (never to the syntax tree the formatter prints), so a stale example
  fails verify like any test, at its place in the comment.
- **`undocumented` is a lint on pub items only**: they are what other files and agents use
  without reading the source. Its first run asked for 32 docs (18 in the standard library); the
  standard library's functions now carry examples that run as tests.

## M25 (v0.26)

- **A public API is a golden**: an agent changing a module can break its users without any test
  failing in the module itself, so the interface is approved like output, and every change,
  even an addition, is seen once before it is approved.
- **The API is what users must write**: signatures with parameter names (calls name their
  arguments, so a rename breaks callers), contracts, fields, variants, interface methods, units;
  not docs, bodies or private items. Lines are sorted, so moving items is no change.
- **A change says whom it breaks**: verify names the project files whose programs include the
  module, from the files each one read, so a user that no longer compiles is still named.
- The standard library's API is approved with the rest of the project, so a change to it is a
  decision, recorded in `golden/`.

## M26 (v0.27)

- **Work is a contract, in steps**: an agent that makes a function quadratic breaks no test, so
  the bound is declared (`steps 2 * xs.len() + 3`) and checked like requires and ensures. Steps
  are the interpreter's deterministic unit, so a bound is exact on every machine, with no timers
  and no tolerance.
- **Checked by verify, not at run time**: native code does not count steps, and the backends
  must agree on every program; the contract lint already runs each function on generated
  arguments, so it measures each call against its bound there.
- **A bound also says what may be slow**: arguments whose bound is beyond what a check may run
  are skipped, not reported, so `std.text.repeat` no longer needs an allow for huge `n`.
- **`steps` is contextual**: it only starts a clause after a signature, so the many programs
  that name a variable `steps` keep working.
- The bound is part of the signature, so the API goldens caught that the standard library's
  promises changed; they were approved with the bounds.

## M27 (v0.28)

- **A bound is measured, then checked**: `anvil cost` fits exact step counts and prints a clause
  that holds for every call it made, so an agent pastes a bound instead of guessing one, and
  verify then holds the function to it.
- **Few forms, one size at a time**: constant, linear and quadratic in one parameter's size,
  with integer coefficients, choosing the simplest that fits about as tightly as the best. They
  are what a reader and a `steps` clause can say; anything else is reported as a loose fit.
- **Eight items were too few to tell growth apart**: property tests keep arrays to 8 items, so
  a quadratic function hid under a linear bound with a large constant. Bounded functions are
  now checked on arrays up to 40 long, and a bound whose growth class is below the measured
  steps' is a finding even while it holds: it is the same bug, met later.

## M28 (v0.29)

- **Memory is a model, like steps**: the interpreter counts bytes by fixed rules (text by
  length, 8 per array element, 16 per map entry, a shared array or map copied before a write),
  so the numbers are exact on every machine and track what the native runtime copies, without
  allocator noise or timing. They are not the native heap's byte count, and the spec says so.
- **One hook, not sixty**: an expression's result is counted where it is evaluated, for the
  kinds the checker already treats as allocating (`noalloc` refuses the same ones), plus the
  copy-on-write sites; values passed through (`get`, `to_bytes`) count nothing.
- **Measuring does not stop at a bound**: `cost` reports bounds without holding calls to them,
  so a broken bound shows how far it is exceeded instead of cutting the curve short.
- **Repeated copying gets its own form**, `n * (n + 1) / 2`: it is the shape of `s = s + x`, and
  whole multiples of `n * n` overshoot it twice over, which read as a loose linear fit.
- **The standard library was the first user**: measured, `repeat` copied its whole result every
  round; it and the padding functions now join once and `words` slices once per word, with
  `allocs` bounds and API goldens so they stay linear. Measuring also found that a call one
  step over its `steps` bound was let through; it is now a finding.

## M29 (v0.30)

- **Fix the common case in the language, not in every program**: `s = s + x` in a loop is the
  first thing anyone writes to build text, and it was quadratic. It now appends in place, so the
  obvious program is also the efficient one; rewriting it with `join` is no longer needed.
- **Only when nobody can tell**: the text grows in place only while the local holds the only
  reference (the runtime's count is 1) and no part reads the local, so value semantics hold
  exactly; anything shared is copied first, as before.
- **Room doubles, kept in the object's kind**: growable text is a kind of its own, sized to a
  power of two above its length, so the room is known from the length alone and no field is
  added to every str; everything else treats it as a str.
- **One rule, two backends, one model**: `ir::append_parts` decides for the interpreter and the
  native code alike, and the byte model counts what the runtime does, so the tools' numbers
  stay true after the optimization.

## M30 (v0.31)

- **Value semantics without the copies**: passing a value on and taking it back (`xs = f(xs)`)
  is how value-semantics code is written, and each call copied the whole value because the
  caller still held it. The last read now moves the value, so the obvious code is linear.
- **Marked once, in the IR**: a liveness pass after checking turns last reads into
  `ExprKind::Move`; the interpreter takes the value out of the frame and the native code takes
  the reference and leaves the local null. Neither backend decides anything, so they cannot
  disagree, and the byte model sees the same moves.
- **Conservative where it is cheap to be**: only reads in plain statements move, and only when
  the statement names the variable once, so evaluation order inside a statement never matters;
  loop counters and snapshots stay live through their loops, and `ensures` keeps what it reads.
- **Say where the copies are**: `profile` counts the bytes copied because a value was shared,
  per function, so an agent finds a quadratic copy by measuring instead of reasoning about
  references.

## M31 (v0.32)

- **State lives in structs**: programs keep their data in fields and arrays of structs, so
  `xs = f(xs)` being linear was not enough: `s.items = f(s.items)` still copied the field every
  call. The corpus program went from 920 MB of copies to 72 bytes.
- **Places, not values, are compared**: a read is taken when no other read in the statement can
  reach the same storage. Fields tell places apart; indices only when they are the same
  variable or literal, so the rule never needs to know a value at run time.
- **Taken the way a write goes**: a take walks the place as the assignment will (arrays made
  unique), so it costs nothing the write would not, and an element is only taken when the
  statement writes it back; past the written place only fields are followed, since an index
  there could copy a shared array the write then throws away.

## M32 (v0.33)

- **A number is not a fix**: knowing a function copied 900 MB still left an agent reading code
  to find which line and which caller. `profile` now says the line that copied, the call that
  handed the value in, and why that argument was not moved, so the fix is the next edit.
- **The copy is in the callee, the cause in the caller**: a push copies because the caller
  still holds the array, so copies are grouped by the call the function was running for, and
  the reasons come from the caller's statement.
- **Reasons come from the same pass**: the move pass records why it kept each argument while it
  decides, so the explanation can never disagree with what the backends do. When every
  argument moved, it says the sharing is elsewhere rather than guess where.

## M33 (v0.34)

- **One append rule, wider**: v0.30 appended only `s = s + x` on a local; moves made other text
  the only reference too (a field written back, a parameter returned). Appending whenever the
  left text is handed over makes those linear without a new statement form.
- **The model must know about room**: an append is cheap only into text that has room, which
  only appends make. The interpreter keeps the texts with room by address (weak references keep
  an address from being reused), so it counts the copy the native code makes into new room
  instead of assuming every lone text can grow. Conversions between str and bytes share one
  object natively, so they carry the room when the old value is gone and drop it while shared.
- **Tests move too**: leaving test bodies unmarked made them a second semantics to reason about
  and left `profile` without reasons there.

## M34 (v0.35)

- **Resources get goldens**: output goldens catch a change in what a program does, nothing caught
  one in what it costs. The interpreter's steps and bytes are deterministic everywhere, so
  they can be approved exactly, with no tolerance and no timing noise.
- **Growth fails, gains pass**: any increase is a finding to look at (and approve when meant); a
  decrease passes but is listed, so improvements are kept by approving them, not lost to the
  next change.
- **Required like goldens**: every file that runs has approved usage, as every main has a golden;
  a file whose test fails is not measured, since its numbers would mean nothing.
- **It guards the compiler too**: the corpus's usage pins what the optimizations of M28-M33 buy;
  switching moves off fails 11 files.

## M35 (v0.36)

- **Check the model against the machine**: every memory number so far came from the interpreter's
  model, and that native code does not copy was only timed by hand. The runtime now counts its
  objects' heap use, so tests can assert it and an agent can see it.
- **Counted by the runtime, not the allocator**: logical sizes (header, length, capacity) rather
  than `malloc`'s, so the numbers repeat exactly across runs and 64-bit platforms. Time is shown
  but never compared. The counters cost one branch per allocation when off.
- **Not part of usage**: approved usage must be checkable without building anything, on every
  `verify`; heap numbers need a native run, so they stay in `measure` and its tests.
- **It paid at once**: comparing the two showed interpolation allocating a string per part.

## M36 (v0.37)

- **Peak is a number now, so act on it**: `measure` showed locals holding values long after their
  last read. Liveness was already computed for moves, so the same walk adds the drops; the rule
  stays in one place and both backends only follow it.
- **A statement, not a step**: `Stmt::Drop` costs no step, so `steps` bounds and approved steps
  cannot change; only bytes copied can, and only downward.
- **Hidden values count**: what `match`, `?` and `for` hold was kept to the function's end, which
  also kept payloads shared. They are written before they are read, so they are no longer live
  before the statement, and go at the first point they can.
- **Not taken from the snapshot**: `for x in xs` still shares each element with what it is
  over, so changing `x` copies it, even when `xs` is dead. A later milestone can hand elements out.

## M37 (v0.38)

- **Ask the users**: thirty-six milestones were chosen by reading the compiler. Three fresh agents
  with only the binary found what reading had not: a crash, a silent native stack overflow, and that
  all three misnamed the golden input file and approved a wrong run.
- **Names by scope**: one name per function was stricter than "no shadowing" needs. It cost about
  twenty renames across the three programs and caught nothing: a name in a block that has ended
  cannot be confused with one in the next.
- **`A | B` over a catch-all**: a catch-all would hide a new variant, the reason `match` has none.
  An arm naming several keeps the check and ends the boilerplate, which the `unreached` lint made
  expensive (60 findings on one program). It binds nothing, so what a binding means stays simple.
- **One field, any name**: constructors already take a single field unnamed, so `Some(x)` was the
  one place it had to be `Some(value: x)`. With several fields, order would be a guess: still named.
- **`panic` is not tested**: a test cannot pass through one, so asking for a test was asking for an
  `allow`. A branch that ends in `panic` states what `allow unreached` only excused. `expect` is a
  library function with a required reason, not a built-in `unwrap`.
- **An argument handed over goes last**: reading a variable has no effect and cannot panic, so doing
  it after the other arguments cannot be seen, except that it can then hand the value over. The rule
  is in `ir::arg_comes_last` and `moves` relies on it; other reads keep their order.
- **Mutants see only what can see them**: a run that never enters the mutated function goes exactly
  as it did, by determinism, so skipping it loses nothing. The budget per run replaces one for the
  whole program, which was computed from the last test alone: any bigger test ran out of steps and
  "noticed" every mutant, so reports of nothing surviving could be false. Rerun under the fix, the
  standard library still has no survivors; the JSON tool, which had reported none, has 7.
- **Stack overflow keeps its text on both backends**: the depth differs by nature, so the interpreter
  no longer names one; a program that runs out on both now has one golden.
- **Left out**: constants, `ln` and `exp` (not the same bits on every platform), sampling profiles
  and a faster interpreter. Listed in M38 or waiting for a second report.

## M38 (v0.39)

- **Constants are their values**: each use is the literal, worked out when checked, so nothing runs
  before `main`, no order of initialisation exists, and an overflow is a compile error. Arrays are left
  out: as values they would allocate at each use, and as anything else they would be a new kind of thing.
- **`for` hands elements out only when alone**: reading the count at run time keeps value semantics
  without an analysis, and the loop variable is now dead at the end of a round, so its last read moves.
- **`let ... else` must leave**: a value has to exist after it, and a default is what `if` is for. It
  is a `match` underneath, so moves, coverage and both backends needed nothing new. No pattern on the
  left: the type is stated as in every `let`, and the only question is whether there is a value.
- **`if` as a value holds one expression a branch**: statements inside would bring blocks with results.
  It is a branch to the `unreached` lint, or the lint would lose what it had when the choice was a `var`.
- **An assert shows what `print` can write**: arrays and structs would need a printer in the runtime
  for every type, as the interpreter has; left for when it is asked for again.
- **Bare arms ignore fields**: `A | B =>` already did, so `A =>` being an error was only a rule to learn.
- **Coverage waits for the tests**: a failing test stops early, so what it would have run looks unrun.
  Thirty false findings are worse than none while the tests are red.
- **Logarithmic needs its own proof**: slack alone took appending, which doubles its room, for
  logarithmic, as a large multiple of the bits rides just above what doubles in steps. So doubling the
  size has to add, not multiply, and a constant bound above what 64 bits can need is not outgrown.
- **Golden text says `panic:` per line**: panics can now run over lines, and a native run's panic is
  read from the last line that starts one. A `panic("a\nb")` could not be approved before this.
- **Left out**: tuples (a struct says more, and no agent lost more than typing), `import as` and raw
  strings holding a backtick (one attempt each). Listed in M39 or waiting for a third report.

## M39 (v0.40)

- **The counts are the check**: an optimisation of the reference interpreter must not change what a
  program does or what it is said to cost, so every approved step, byte and copy count had to stay
  exactly as it was, on this repository and on the six agents' programs. That is what made it safe to
  change how values are held.
- **Faster, not a bytecode**: reading a field cloned the whole struct and a literal allocated its text
  each time, so most of the cost was waste, not the tree walk. Three times faster for a few hundred
  lines; a compiled form is the next step if the loop is still what agents complain of.
- **Fields are shared until written**, as arrays were: a struct held in another value is read without
  copying. Arrays inside count as shared exactly when they did, since writing unshares on the way down.
- **Mutants need only what runs them**: a run that does not run the mutated statement goes as it did,
  so it cannot notice. Verdicts were compared with the old way on all six programs: none differ. A
  survivor nothing runs is told apart from one that was run and not noticed: they want different fixes.
- **Values beside the panic**: writing any value natively would need a printer for each type, and the
  panic text must agree on both backends. Tests run in the interpreter, which has one, so it adds them
  beside the panic: where they are wanted, with the rule intact.
- **`testkit.anv` by name**: a new visibility or `test fn` would be language for one lint's sake. A
  file's name says what it is for, and its helpers still have to be tested.
- **Run speed comes from what a program does most, not from a new backend** (v0.41). A sampling profiler
  and the machine code named the costs. Cranelift stays: a build still takes a fraction of a second.
- **Maps are hashed underneath, sorted when looked at.** A tree would keep them sorted all the time at
  the price of every lookup. Up to 32 elements a map is the sorted array it always was; past that a
  table finds keys, new elements go on the end and whoever walks the elements settles them first. Settling
  merges what is new, so it costs no more than the walk it comes before. The hash is fixed and order
  never depends on it: determinism is untouched, and the interpreter keeps its sorted vector, as only
  time differs.
- **Pools, not a general allocator.** Every object's size is known when it is freed, so a free list per
  size needs no headers. Chunks are never given back: a program's small-object memory stays at its peak.
  A worker's pool goes to the next worker, so repeated `parallel_map`s do not grow.
- **One owner, proved by emission order.** `unique.rs` says which locals a statement might share or
  replace, and `Gen` forgets those around it; a check counts only where it dominates what follows (not in
  one arm, not in part of an expression, not after a loop that may not run). `sole_owner.anv` exists
  for this: breaking any of these rules on purpose makes its golden fail.
- **Cranelift's inliner copies data symbols as they are**, so every function shares one table of
  names (`share_names`). Without it programs read the wrong data; it is what the first attempt found.
- **Not done:** signed add and subtract still set a byte and test it, as Cranelift folds that only
  for multiply and unsigned add; no vectorizing, which is most of what is left on matrix code; spilling in
  very large functions.
- **`anvil hot` samples the real run** (v0.42). `profile` is exact but counts steps in the interpreter,
  and agents in the benchmark rebuilt and timed their way to speed: 54 tool calls for one ledger. So the
  command names the program's functions and what the runtime does in plain words, with what to do about
  it. Its numbers differ per run and nothing compares them, which is why it is not part of `measure`.
- **Proofs are about locals, not values.** "This local is a good index of that array" is simple to keep
  right around statements, arms and loops, by the same rules as one owner. Reasoning about `i + 1` or
  `while i < n` would catch more and could be wrong in more ways.
- **Loops do not read array lengths and storage ahead.** It was built, looked like 19% run after run, and
  gave nothing once measured properly (none 1.059x, innermost 1.040x, all loops 1.050x against v0.41), as
  the held values take registers from what is hotter. Removed. A checked access costs little on a wide
  CPU: what is left is cache misses, chains of float additions, and locals the allocator keeps on the stack.
- **Timing needs one fast core.** This CPU has fast and slow cores, and other work shares a core's second
  thread, so even builds run in turn moved by 2x and agreed on a gain that was not there. A comparison
  now runs pinned to one fast core at high priority, with the core's other thread kept idle.
- **Stdin is not sought.** Seeking discards what `read_line` left buffered: a golden caught it. Only a
  file just opened gets the size shortcut.
