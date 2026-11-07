# Roadmap

The first unchecked milestone is current work. Each milestone ends with `cargo test --workspace`
green and the interpreter and native backend agreeing on every program in `tests/programs`.

## M0: Core language (v0.1)
- [x] Lexer, parser, checker with stable error codes and typed fixes
- [x] Types: i64, f64 with units, bool, str, structs
- [x] Effects (`uses io`), contracts (`requires`/`ensures`), `test` blocks
- [x] Reference interpreter
- [x] Native backend: Cranelift, embedded C runtime, MSVC link, reproducible executables
- [x] CLI: check, run, build (`--asm`), test, explain, spec; `--json` everywhere
- [x] Parity corpus, error corpus (every code tested), mechanical-fix resolution tests

## M1: Data (v0.2)
- [x] Growable arrays `[T]` with value semantics, bounds-checked indexing, `push`, `pop`, `len`
- [x] `for x in array`
- [x] String values: interpolation, `+`, comparison, `len`
- [x] Enums with payloads, exhaustive `match` with no catch-all
- [x] `Option[T]`, `Result[T, E]` and `?`; errors are values, no exceptions
- [x] Native backend: reference counting with copy-on-write, leak check in every native test

## M2: Agent surface (v0.3)
- [x] `anvil_ops`: one implementation of every command, returning the JSON both faces send
- [x] `anvil-mcp`: every command as an MCP tool over stdio; failing checks are tool errors
- [x] Parity test: commands and tools match both ways, with the same arguments
- [x] `anvil outline`, `anvil type-at`, `anvil refs`, answered from the checker even with errors
- [x] `anvil fix`: mechanical fixes applied until none is left
- [x] Step limit for `run` and `test`

## M3: Projects, goldens and the record (v0.4)
- [x] `anvil.toml` projects and `anvil list`
- [x] Goldens: `golden check` (interpreter, and `--native` with leak checks) and `golden approve`
- [x] The project record (SQLite): milestones from the roadmap, decisions, findings with history,
      verify runs; `record query/decide/status/import-roadmap/export`
- [x] `verify` syncs findings into the record; `review` writes a sign-off sheet
- [x] The repository is an Anvil project: its parity corpus runs through `golden check --native`

## M4: Watching programs run (v0.5)
- [x] `anvil trace`: calls with their arguments, returns and prints, as Anvil values, bounded
- [x] `anvil profile`: calls and steps per function, deterministic
- [x] The `untested` lint in `verify`, with `allow LINT "reason"` before a function; stale allows
      are findings
- [x] `golden check` leaves the actual output and a diff in `out/golden/`
- [x] `anvil watch` (CLI only, listed as a parity exception with its reason)

## M5: Formatter (v0.6)
- [x] `anvil fmt` with one canonical layout, comments preserved
- [x] `anvil check` reports unformatted files as an error with a mechanical fix
- [x] Parser reports several syntax errors per run where recovery is reliable

## M6: Text and collections (v0.7)
- [x] Slicing arrays and strings by range, string search and split
- [x] Maps with deterministic iteration order
- [x] `==` for arrays, structs and enums whose parts all have it

## M7: Modules (v0.8)
- [x] Multiple files, explicit imports by name, no wildcard imports
- [x] Visibility (`pub`), one module per file

## M8: Generics (v0.9)
- [x] Generic functions and structs with explicit, use-site-visible constraints

## M9: Verification (v0.10)
- [x] `anvil test --native`
- [x] Differential fuzzing: random well-typed programs, interpreter vs native
- [x] Property tests in the language (`for all` over generated inputs, seeded)

## M10: Platforms (v0.11)
- [x] Linux x86-64 (ELF): the whole suite, every golden on both backends and fuzzing pass
- [ ] Linux AArch64 and macOS x86-64/ARM64 (Mach-O): written, to be run by the release workflow
- [x] Release binaries: `scripts/release.sh` and the tag-driven release workflow

## M11: Language server (v0.12)
- [x] Language server on the same facts as `type-at` and `refs`, plus completion: `anvil lsp`
  (diagnostics, hover, definition, references, completion, formatting, symbols, fixes as code
  actions), `anvil complete`, `refs` by position, and facts that follow imports into modules

## M12: Effects and interop (v0.13)
- [x] Effects for allocation, time and seeded randomness: `uses clock` (virtual in tests and
  goldens), `uses random` (one specified, seeded sequence) and the `noalloc` promise
- [x] C FFI with declared effects: `extern` functions from the C runtime or a C file, on both
  backends

## M13: Programs that take input (v0.14)
- [x] Arguments and standard input: `args()`, `read_line()`, given by `anvil run -- ARGS`,
  `--input FILE` and a golden's `.in` file
- [x] Files under a new `fs` effect: `read_file`, `write_file` and the `FileError` enum; checked
  runs read the disk and keep their writes in memory, on both backends
- [x] Reading values from text: `trim`, `parse_i64`, `parse_f64`

## M14: Function values (v0.15)
- [x] Function types `fn(T) -> R uses ... noalloc`; declared functions as values
- [x] Function literals capturing by value, on both backends (closures natively, with leak checks)
- [x] Effects and noalloc in function types, with a function that does less fitting a type that
  allows more; generics over function types (`map`, `keep`, `fold`, `sort_by` in Anvil)
- [x] The fuzzer makes function literals and passes helpers as values

## M15: Methods and interfaces (v0.16)
- [x] Methods on structs and enums, `fn Type.name(self, ...)`, found by a value's type across
  modules; methods of generic types and generic methods
- [x] Interfaces as constraints (`T: Shape`), met by having the methods, with `Self`
- [x] Outline, completion, refs and traces name methods `Type.name`

## M16: Bytes and bits (v0.17)
- [x] Bit operators `& | << >> ~`, `xor`, `unsigned_shr` and wrapping arithmetic on i64; hex and
  binary literals; `^` refused with a fix (E0393)
- [x] The `bytes` type: `bytes_of`, indexing, slices, joining, `to_str`, `hex`, `to_array`, and
  binary files (`read_bytes`, `write_bytes`), on both backends
- [x] The fuzzer uses bit operations; property tests generate bytes

## M17: Standard library: text, collections and the system (v0.18)
- [x] Text: `replace`, `to_upper`, `to_lower`, `chars`, `join`, and floats with fixed decimals
- [x] Arrays: `sorted` (stable) and `reversed`
- [x] The system: `eprint` (stderr kept in goldens), `exit`, `env` (given by a golden's `.in`)
  and `list_dir` (with writes kept in memory), on both backends

## M18: Publishing (v0.19)
- [x] Every crate packages for crates.io (`anvil-lang` and `anvil-lang-*`) and passes
  `cargo publish --dry-run`; a binary built from the packages alone works
- [x] Docs site (`anvil docs`), contribution guide, changelog, CI workflow (Windows and Linux)
- [ ] Publish: a repository URL, the commit email in the history, and the upload itself are the
  owner's decisions; the CI workflow has not run yet

## M19: A standard library in Anvil (v0.20)
- [x] `std` modules written in Anvil and built into the compiler, imported as `import std.list.{map};`
  from any file: `std.list`, `std.set`, `std.text`, `std.math`
- [x] Each has tests and property tests, and is part of the repository's project, so verify holds
  it to the same rules as any code (every function tested, canonical layout)
- [x] `anvil std` (modules and their pub items, or one module's source), a docs page, `\r` in strings

## M20: Parallel work (v0.21)
- [x] `xs.parallel_map(f)`: a map over worker threads with the result of a map in order; `f` must
  be pure (E0416), so the result is the same on every run and on both backends
- [x] A panic reports the first element that panics, as a loop would; elements, results and
  captures of any type; nested parallel work; `ANVIL_THREADS` sets the number of workers
- [x] The fuzzer uses it, so the backends are compared on it

## M21: Branch coverage (v0.22)
- [x] The `unreached` lint in verify: an `if` or `else` block, match arm or loop body of a function
  that runs, which no test or golden-checked main takes; `allow unreached "why"` with stale allows
- [x] Counted in the interpreter by markers the checker puts at the start of each branch (native
  code ignores them); instances share their generic's branches, function literals count for the
  function around them, empty blocks are not branches
- [x] Per-file branch counts in `verify --json` and the review sheet; the gaps it found in the
  standard library and the corpus are covered by new tests

## M22: Mutation testing (v0.23)
- [x] `anvil mutate FILE [--only NAMES]`: operator, literal, `!`/`-`/`~` and call-statement
  mutants, each run against the tests and goldens of the file and of every project file that
  imports it, with step budgets from the unchanged run; CLI and MCP
- [x] Killed, survived and invalid mutants with what killed each; `allow mutants "why"` for
  equivalent ones, stale when nothing survives; an `anvil explain mutants` entry
- [x] The standard library is mutation-clean: the survivors it found have tests or reasoned allows

## M23: Contracts checked without tests (v0.24)
- [x] The `contract` lint in verify: each pure function with a requires or ensures is called on
  100 generated arguments; one its requires accept that makes it panic or break its ensures is
  a finding, shrunk and shown as Anvil values; so are calls past a million steps and requires
  that accept nothing; `allow contract "why"` with stale allows
- [x] Overflow of i64 counts as excluded input; functions with effects or function parameters
  are listed as unchecked in `verify --json`
- [x] What it found is fixed: `pow` by squaring (it looped `exp` times), the projectile
  example's missing requires (negative mass, zero `dt`)

## M24: Documented interfaces (v0.25)
- [x] Doc comments: the `//` lines above an item, field or variant, shown with the signature by
  outline, std, type-at, completion, hover and the docs site
- [x] `// example: EXPR` doc lines are tests of the file, checked and run like any test (both
  backends, coverage, mutation testing), with errors reported in the comment
- [x] The `undocumented` lint for pub items; the standard library and the corpus modules are
  documented, the standard library's functions with examples

## M25: API goldens (v0.26)
- [x] `anvil api [FILE]`: each file's public API, one sorted line per pub item, docs left out,
  against the approved `golden/FILE.api`; added, removed and changed items
- [x] `anvil api approve FILE` (removes the approved API when no pub items are left); CLI and MCP
- [x] verify reports every difference and unapproved APIs, naming the project files a removal or
  change breaks; the standard library's and the corpus modules' APIs are approved

## M26: Step bounds (v0.27)
- [x] `steps EXPR`, the last contract clause: an i64 of the parameters bounding a call's steps
  (statements and loop iterations, callees included); `steps` stays a name everywhere else
- [x] Checked by the contract lint on generated arguments, never at run time (native code does
  not count steps): a call over its bound is a finding, shrunk; arguments bounded beyond what a
  check may run are not run
- [x] Bounds on the standard library's `sum`, `concat`, `unique`, `pow`, `gcd`, `repeat` and
  `words`, approved as part of their APIs

## M27: Cost curves (v0.28)
- [x] `anvil cost FILE [--only NAMES]`: each pure function on 200 generated argument lists of
  growing size, its steps fitted as constant, linear or quadratic in one parameter's size; the
  steps at each size and the tightest `steps` clause that holds, ready to paste; CLI and MCP
- [x] Bounded functions are checked on wider arguments (up to 40 long, not 8), and a bound that
  grows slower than the steps it bounds is a contract finding, though it holds for every size
  checked

## M28: Memory (v0.29)
- [x] Bytes allocated, counted by the interpreter by a fixed model (lengths of new text, 8 per
  array element, 16 per map entry, copies of shared values), per call and per function
- [x] `profile` shows each function's bytes; `cost` fits bytes like steps (with the triangular
  `n * (n + 1) / 2` of repeated copying) and suggests `allocs` clauses, measuring past bounds
- [x] `allocs EXPR`, a contract clause checked by the contract lint like `steps`, outgrown bounds
  included; a call one step over its `steps` bound is now caught (it returned unreported)
- [x] The standard library's text functions made linear in memory: `repeat` (2,259,840 bytes at
  its costliest call, now 16,640), `pad_left` and `pad_right` (quadratic in width), `words`
  (a str per character); their bounds approved as part of their APIs

## M29: In-place text building (v0.30)
- [x] `s = s + a + b` on a str or bytes local whose parts do not read it appends in place while
  the local holds the only reference: growable text in the runtime (room that doubles), one
  rule (`ir::append_parts`) for both backends; shared text is copied first
- [x] The byte model counts appends as the runtime does, so `profile`, `cost` and `allocs` stay
  true; prepending is still quadratic, and is the example of repeated copying
- [x] A corpus program for aliasing, sharing, self-reference, chains and bytes; the fuzzer
  appends; building 400,000 characters natively takes 12.3 s by copying and well under 0.1 s
  by appending

## M30: Moves (v0.31)
- [x] The last read of a variable moves its value (`ExprKind::Move`, marked by one liveness
  pass, `moves::mark`, through loops, branches, break and continue): the receiver may hold
  the only reference, so `xs = add(xs, x)` changes `xs` in place in both backends; values
  `ensures` reads never move
- [x] `profile` shows, per function, the bytes copied because a value was still shared when it
  changed, so the copies a move would avoid are found by measuring, not by reading
- [x] A corpus program (kept copies, later reads, loops, branches, closures, structs, enums,
  ensures) on both backends with the leak check; the fuzzer hands values to calls and back;
  100,000 updates natively take 18.8 s when a copy is kept and 10,000,000 take 0.5 s moved

## M31: Moves out of places (v0.32)
- [x] The last read of a field or an element moves too (`ExprKind::Take`, marked by the same
  pass): when the statement writes the place again (`s.items = add(s.items, x)`,
  `rows[i] = add(rows[i], x)`), or the value holding it is dead and the place is reached by
  fields (`s = State { items: add(s.items, x), n: s.n + 1 }`); places that may be the same
  (another index, the place read twice, a place around it) are still shared
- [x] Both backends take the value and leave the place empty, reached as a write would reach it
  (arrays on the way made unique), so a place left empty by an early `?` return is released
  with its value; the byte model and `profile` count the same
- [x] A corpus program (fields, rebuilt structs, elements, fields of elements, kept copies,
  same-element indices, early returns, maps, text and bytes) on both backends with the leak
  check; the fuzzer updates fields, rebuilt structs and elements; `cost` finds field and
  element updates linear; 10,000,000 field and element updates natively take 0.5 s, 100,000
  with a copy kept take 69 s

## M32: Copies located (v0.33)
- [x] Calls, assignments and in-place changes (`push`, `pop`, `set`, `remove`) carry their
  site; the interpreter counts each copy of a shared value at the site that made it, and by
  the call the function was running for
- [x] The move pass lists each call's arguments read from variables and places, and why each
  is not moved (`Program::handed`): named again, read again later, a place the statement may
  write, an element not written back, read by `ensures`
- [x] `profile` lists the copy sites, costliest first, with their lines, and under each the
  calls that handed the shared values in and what their arguments did, in text and `--json`
  (`copies`); a corpus test pins the site, the call and the reason

## M33: Text appends everywhere (v0.34)
- [x] `a + b` appends to `a`'s text when `a` is handed over (a move or a take) or itself such an
  append (`ir::concat_appends`), in both backends: `return s + x`, `p.name = p.name + x` and
  `s + a + b` on a moved `s` grow text in place, so building text in a field or through a
  function is linear
- [x] The byte model knows which text has room, as the runtime does (text made by appends; kept
  across `to_bytes` and `to_str` when the old value is gone): the first append to other text
  counts the copy the native code makes, closing the gap left in v0.30
- [x] Tests are move-marked like functions, so they run as functions do and `profile` gives
  reasons for their calls
- [x] A corpus program (fields, bytes, functions, chains, kept copies, static text, conversions,
  a test) on both backends with the leak check; the fuzzer appends to text in fields; `cost`
  finds text built through a function linear and counts the first copy; natively 10,000,000
  field and function appends take 0.84 s, 100,000 with copies kept 1.5 s

## M34: Usage baselines (v0.35)
- [x] `golden/FILE.usage`: the steps, bytes and copied bytes of each run of a file (main under
  its golden's input, each test), approved like goldens; `anvil usage` compares, `anvil usage
  approve` stores (every changed file without a FILE), in the CLI and over MCP
- [x] `verify` reports any growth, a run not approved yet, one that no longer runs, and a file
  with runs but no approved usage; less passes and is listed so the gain can be approved
- [x] Every corpus program, example and standard library module has its usage approved, so a
  change to the compiler that costs programs more is found: with moves switched off, 11 of 73
  files fail (place_moves.anv's bytes grow from 256,633 to 920,136,769)

## M35: Native measurement (v0.36)
- [x] The runtime counts what objects ask of the heap when `ANVIL_MEASURE` names a file: bytes
  asked for, the most held at once, objects made
- [x] `anvil measure [FILE]` builds and runs programs under their goldens' input, one worker at
  a time, and shows those numbers and the time beside the interpreter's counts, in the CLI and
  over MCP
- [x] Native memory is tested, not only timed by hand: moved updates and text in a field are
  linear, kept copies quadratic, a loop's temporaries are given back, and one small program's
  numbers are pinned exactly
- [x] What it found: each `{part}` of an interpolation built a whole new string, so one with many
  parts was quadratic. It now allocates its result once (floats_sweep.anv: 120,002 heap bytes
  to 49,842, 2,080 objects to 960)

## M36: Dead values let go (v0.37)
- [x] A local lets go of its value when nothing can read it again (`Stmt::Drop`, not a step):
  after the statement that read it last, at the start of a branch that does not read it, after
  a loop that did; never what `ensures` reads
- [x] Moves in conditions: the condition of an `if`, what a `match` or `for` is over; what a
  `match` or `?` was over goes once the payload is out, so the payload is not shared with it
- [x] A corpus program on both backends with the leak check; native peak is tested from real
  heap numbers (two big arrays in turn peak at one, in seven shapes of last read); across the
  corpus peak falls 5%, place_moves.anv's by half (262,240 to 131,229 bytes)

## M37: Proving ground (v0.38)
- [x] Three fresh agents, given only the v0.37 binary, wrote a JSON query tool, a Lisp interpreter
  and a warehouse simulation, each to a clean `verify --native`, and logged what cost them time
- [x] Bugs: a crash on a byte-order mark or a wide character outside a string; an `allow contract`
  called stale when its contract could not be checked at all; a native stack overflow ending the
  program with no word (now the same panic as the interpreter's, in workers too)
- [x] Names can be reused side by side (match arms, two loops); the only field of a variant takes
  any name (`Some(x)`, `Err(_)`); one arm can name several variants (`A | B => { }`)
- [x] `panic(message)`, whose branches need no test; `std.expect`; raw strings in backticks;
  `print_raw`; `read_stdin`
- [x] Hidden copies gone: `f(s) + x` appends, and an argument handed over is evaluated last, so
  `xs = add(xs: xs, x: xs.len())` does not copy
- [x] `mutate` runs mutants side by side, each only against the runs that enter its function, with
  a step budget per run (the old one came from the last test alone, so a bigger test falsely
  "noticed" every mutant), stops at the first that notices, and says how far it is on stderr
- [x] Contracts tying arguments together are reached: if too few inputs pass `requires`, more try
  small numbers within the arrays' lengths
- [x] Files in `golden/` that belong to no program are found (all three agents misnamed the input
  file and approved a run without it); `measure` takes arguments and `--input`; errors point at
  `std` functions and at the line missing its `;`; `--help` is not an error

## M38: Proving ground, second round (v0.39)
- [x] Three fresh agents, given only the v0.38 binary, wrote a Markdown converter, a chess move
  generator and a SQL engine over CSV, each to a clean `verify --native`, with no compiler bug found
- [x] What the first round asked for: `for x in xs` hands out the elements of an array nothing else
  holds; constants; `profile` by line, and naming the call further up that kept a reference; `cost`
  on struct fields, one generic instance and logarithms (`bit_len`); `measure --native-only`
- [x] A failed `assert` on numbers, booleans or text shows both sides, on both backends (the most
  costly thing in two of the three logs)
- [x] `let x: T = e else { ... };` and `if c { a } else { b }` as a value, so a choice or an unwrap
  needs no placeholder `var`; a variant named alone in an arm ignores its fields; character literals
- [x] `allow mutants` on one statement; constants are mutated; failing tests hold back the coverage
  lints; errors saying the same thing are counted, not repeated; `usage` shows numbers before the
  first approval; a golden input at the wrong depth is named when approving; the spec shows how a
  test declares effects

## M39: Speed of the loop (v0.40)
- [x] The interpreter, which every test, golden, `verify` and mutant runs on, is about three times
  faster on six real programs, with every approved step, byte and copy count unchanged: reads in place
  instead of copies, fields shared until written, frames used again, numbers and truth without going
  through a value, and the common cases out of the one large function
- [x] `mutate` runs only what runs the mutated statement, and says when nothing does
- [x] `verify` checks files side by side
- [x] A failed `assert` on arrays, structs and the like says what they were, in a test
- [x] `s.find_from(t: x, at: i)`; a `testkit.anv` shares test helpers without being an API

## M40: Run speed (v0.41)
What the benchmark against Rust, C++ and Java found: right answers everywhere, the fastest builds, the
slowest programs, and agents spending their effort working around that.
- [x] Big maps find keys by hash and stay sorted for whoever looks: a map with 312,527 keys built in
  a fraction of the time; order, equality and value semantics as before
- [x] Small objects come from per-thread pools, so a program that allocates is no longer limited by
  the system's allocator; objects are counted for leak checks only when those run
- [x] Reference counts, array growth, shifts and byte reads are inline, with the runtime on a cold path
- [x] An array known to have one owner needs no check before a write, which also keeps a loop's
  values in registers; length and storage are not read again after a write to an element
- [x] Small functions are inlined
- [x] `find`, `split` and turning integers into text are faster, and output goes out in larger pieces

## M41: Run speed, second pass (v0.42)
Agents in the benchmark tried speed-ups blind, build after build. This gives them the measurement, and
takes the next layer of cost out of plain code.
- [x] `anvil hot FILE`: a compiled program's time by function and by what the runtime does for it, from
  samples of the real run, and which arrays a loop keeps checking for a second owner
- [x] A local that passed a bounds check stays a good index until it or the array changes, and a counter
  up to an array's length needs no check at all
- [x] A counter plus 1 cannot overflow; adding a number takes one compare-and-branch
- [x] `rows[i][j]` reads without touching the reference count of the row
- [x] Small arrays made at their full size are one allocation, with their elements beside their header
- [x] A file is read straight into its text, at its known size

## M42: Run speed, third pass (v0.43)
Fewer checks repeated round after round, and a way for agents to tell a real gain from a lucky run.
- [x] A loop sure to write an array checks for a second owner once, before it starts, and copies only
  what its first round would have copied
- [x] A map finds the key it just found without hashing it again
- [x] `anvil compare A B`: which of two programs runs faster, on one fast core, and whether that is more
  than the runs move on their own; output must match
- [x] Workers running out of stack at once report it once

## M43: Proving ground, third round (v0.44)
- [ ] Fresh agents on new kinds of program, against the same log format: do `let ... else`, `if`
  values and constants get used, and is the loop faster for them?
- [ ] What round two asked for and did not get: describing a valid struct to the contract lint, so it
  need not be switched off; several goldens per program; tuples
