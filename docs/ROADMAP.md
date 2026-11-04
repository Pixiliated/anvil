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
