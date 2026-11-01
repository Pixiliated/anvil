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

## M14: Publishing
- [ ] crates.io as `anvil-lang` (`anvil` is taken), docs site, contribution guide
