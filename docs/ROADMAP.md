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

## M1: Formatter
- [ ] `anvil fmt` with one canonical layout, comments preserved
- [ ] `anvil check` reports unformatted files as an error with a mechanical fix
- [ ] Parser reports several syntax errors per run where recovery is reliable

## M2: Arrays and strings
- [ ] Fixed arrays `[T; N]` and slices, bounds-checked with located panics
- [ ] `for x in array`
- [ ] String values: length, concatenation, comparison, formatting numbers to strings

## M3: Enums and errors
- [ ] Enums with payloads, exhaustive `match`
- [ ] Recoverable errors as values (a built-in result type), no exceptions

## M4: Modules
- [ ] Multiple files, explicit imports by name, no wildcard imports
- [ ] Visibility (`pub`), one module per file

## M5: Generics
- [ ] Generic functions and structs with explicit, use-site-visible constraints

## M6: Verification
- [ ] `anvil test --native`
- [ ] Differential fuzzing: random well-typed programs, interpreter vs native
- [ ] Property tests in the language (`for all` over generated inputs, seeded)

## M7: Platforms
- [ ] Linux x86-64 and AArch64 (ELF), macOS (Mach-O)
- [ ] Release binaries

## M8: Tooling for agents
- [ ] `anvil query --json`: type at a position, callers of a function, uses of a field
- [ ] Language server

## M9: Effects and interop
- [ ] Effects for allocation, time and seeded randomness
- [ ] C FFI with declared effects

## M10: Publishing
- [ ] crates.io as `anvil-lang` (`anvil` is taken), docs site, contribution guide
