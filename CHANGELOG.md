# Changelog

Each version is one roadmap milestone (docs/ROADMAP.md); docs/DECISIONS.md says why things are
the way they are.

## 0.21.0
- `xs.parallel_map(f)`: a pure map on worker threads, with the same result as a loop; the first
  panicking element is the one reported. `ANVIL_THREADS` sets the number of workers.
- E0416: the function given to `parallel_map` has effects.

## 0.20.0
- A standard library written in Anvil and built in: `std.list`, `std.set`, `std.text`,
  `std.math`, imported as `import std.list.{map};`; `anvil std` lists it, and the docs site has a
  page for it.
- `\r` in strings.

## 0.19.0
- Ready to publish: the crates package for crates.io as `anvil-lang` (the `anvil` command),
  `anvil-lang-mcp` and five library crates, and pass `cargo publish --dry-run`.
- `anvil docs` writes a static site: the specification, every error code and lint, the
  built-ins.
- A CI workflow (Windows and Linux), a contribution guide and this changelog.

## 0.18.0
Text (`replace`, `to_upper`, `to_lower`, `chars`, `join`, `fixed`), `sorted` and `reversed`;
`eprint` with stderr kept in goldens, `exit`, `env` and `list_dir`.

## 0.17.0
Bit operators, `xor`, `unsigned_shr` and wrapping arithmetic; hex and binary literals; the
`bytes` type and binary files.

## 0.16.0
Methods on structs and enums (`fn Circle.area(self)`), interfaces as constraints.

## 0.15.0
Function types, functions as values, function literals that capture by value.

## 0.14.0
Arguments, stdin and files (the `fs` effect, `FileError`); `trim`, `parse_i64`, `parse_f64`.

## 0.13.0
The `clock` and `random` effects, the `noalloc` promise, C functions (`extern`).

## 0.12.0
The language server (`anvil lsp`) and `anvil complete`.

## 0.11.0
Linux x86-64, release binaries.

## 0.10.0
`anvil test --native`, differential fuzzing, property tests.

## 0.9.0
Generic functions and types with built-in constraints.

## 0.8.0
Modules: one per file, imports by name, `pub`.

## 0.7.0
Slices, string search and split, maps, structural `==`.

## 0.6.0
`anvil fmt`: one canonical layout, checked by the compiler.

## 0.5.0
`anvil trace`, `anvil profile`, the `untested` lint, `anvil watch`.

## 0.4.0
Projects, goldens and the project record.

## 0.3.0
The MCP server, `outline`, `type-at`, `refs` and `fix`.

## 0.2.0
Arrays, strings, enums with `match`, `Option` and `Result`.

## 0.1.0
The core language, the interpreter and the native backend.
