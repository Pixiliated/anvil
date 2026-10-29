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

## M3: Formatter
- [ ] `anvil fmt` with one canonical layout, comments preserved
- [ ] `anvil check` reports unformatted files as an error with a mechanical fix
- [ ] Parser reports several syntax errors per run where recovery is reliable

## M4: Text and collections
- [ ] Slicing arrays and strings by range, string search and split
- [ ] Maps with deterministic iteration order
- [ ] `==` for arrays, structs and enums whose parts all have it

## M5: Modules
- [ ] Multiple files, explicit imports by name, no wildcard imports
- [ ] Visibility (`pub`), one module per file

## M6: Generics
- [ ] Generic functions and structs with explicit, use-site-visible constraints

## M7: Verification
- [ ] `anvil test --native`
- [ ] Differential fuzzing: random well-typed programs, interpreter vs native
- [ ] Property tests in the language (`for all` over generated inputs, seeded)

## M8: Platforms
- [ ] Linux x86-64 and AArch64 (ELF), macOS (Mach-O)
- [ ] Release binaries

## M9: Language server
- [ ] Language server on the same facts as `type-at` and `refs`, plus completion

## M10: Effects and interop
- [ ] Effects for allocation, time and seeded randomness
- [ ] C FFI with declared effects

## M11: Publishing
- [ ] crates.io as `anvil-lang` (`anvil` is taken), docs site, contribution guide
