# Changelog

Each version is one roadmap milestone (docs/ROADMAP.md); docs/DECISIONS.md says why things are
the way they are.

## 0.29.0
- Memory: the interpreter counts bytes allocated by a fixed model; `profile` shows each
  function's bytes, `cost` fits them and suggests `allocs` clauses, and `allocs EXPR` bounds a
  call's memory, checked by the contract lint.
- `std.text`: `repeat`, `pad_left`, `pad_right` and `words` allocate linearly (they were
  quadratic, or made a str per character), with `allocs` bounds.
- Fixed: a call that took exactly one step more than its `steps` bound was not reported.

## 0.28.0
- `anvil cost FILE`: how each pure function's steps grow with its arguments, the steps at each
  size, and the tightest `steps` clause that holds.
- Bounded functions are checked on arguments up to 40 long, and a bound that grows slower than
  the function's steps is a contract finding.

## 0.27.0
- Step bounds: `steps EXPR`, a contract clause bounding a call's steps, checked by verify's
  contract lint on generated arguments (shrunk counterexamples); arguments bounded beyond what a
  check may run are skipped.
- Bounds on the standard library's `sum`, `concat`, `unique`, `pow`, `gcd`, `repeat` and `words`.

## 0.26.0
- API goldens: `anvil api [FILE]` and `anvil api approve FILE`; each file's public API is
  approved in `golden/FILE.api`, and verify reports added, removed and changed items, naming the
  files a removal or change breaks.

## 0.25.0
- Doc comments: the `//` lines above an item are its doc, shown by outline, std, type-at,
  completion, hover and the docs site.
- `// example: EXPR` doc lines are tests.
- The `undocumented` lint for pub items; the standard library is documented, with examples.

## 0.24.0
- The `contract` lint: verify calls each pure function with a `requires` or `ensures` on 100
  generated arguments and reports, shrunk, any its requires accept on which it panics, breaks
  its ensures or runs past a million steps; `allow contract "why"`.
- `std.math.pow` squares (at most 63 rounds; it used to loop `exp` times); the projectile
  example states the inputs it takes.

## 0.23.0
- `anvil mutate FILE`: mutation testing. One operator, literal, `!`/`-`/`~` or call statement
  changed at a time, run against the tests and goldens of the file and its importers; survivors
  fail it, `allow mutants "why"` excuses equivalent ones.
- New standard-library tests for what it found (`sign`, ties in `min_by_key`/`max_by_key`,
  one-letter `words`).

## 0.22.0
- The `unreached` lint: `verify` reports each `if`/`else` block, match arm and loop body of a
  tested function that no test or golden-checked main takes; `allow unreached "why"` switches it
  off for a function. `verify --json` and the review sheet count branches taken.
- New tests for the standard-library branches it found untested.

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
