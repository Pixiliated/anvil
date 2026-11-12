<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/Pixiliated/anvil/main/branding/lockup-on-dark.svg">
    <img alt="Anvil" src="https://raw.githubusercontent.com/Pixiliated/anvil/main/branding/lockup-on-light.svg" width="360">
  </picture>
</h1>

A small compiled programming language designed to be written mostly by AI agents and read by
people.

```
unit m;

enum Shape {
    Circle(radius: f64[m]),
    Rect(width: f64[m], height: f64[m]),
}

fn area(s: Shape) -> f64[m^2] {
    match s {
        Circle(radius) => { return 3.14159 * radius * radius; }
        Rect(width, height) => { return width * height; }
    }
}

fn parse_digit(c: str) -> Result[i64, str] {
    if c == "0" {
        return Ok(0);
    }
    if c == "1" {
        return Ok(1);
    }
    return Err("not a binary digit: {c}");
}

fn binary(digits: [str]) -> Result[i64, str] {
    var n: i64 = 0;
    for d in digits {
        let bit: i64 = parse_digit(d)?; // returns the Err early
        n = n * 2 + bit;
    }
    return Ok(n);
}

fn main() uses io {
    let shapes: [Shape] = [Shape.Circle(1.0[m]), Shape.Rect(width: 2.0[m], height: 3.0[m])];
    for s in shapes {
        print("area {area(s)}"); // area 3.14159 m^2, then area 6.0 m^2
    }
    match binary(["1", "0", "1"]) {
        Ok(value) => { print("binary 101 is {value}"); }
        Err(error) => { print(error); }
    }
}

test "rectangles" {
    assert(area(Shape.Rect(width: 2.0[m], height: 2.0[m])) == 4.0[m^2]);
}
```

## Why another language

Agents write code quickly. They are worse at knowing when that code is right. Anvil moves as
much of that judgement as possible into the compiler:

- **One obvious way.** One loop per job, one way to bind, one way to call, one layout (`anvil
  fmt`; any other layout is an error whose fix is the formatted file). Variation between runs
  and between agents shrinks, and so does the room for plausible-but-wrong code.
- **Errors that carry their fix.** Every error has a stable code, an exact range, and fixes
  labelled `mechanical` (safe to apply) or `changes_meaning` (compiles, but may hide the real
  bug). `--json` gives the same thing to tools.
- **Local and explicit.** No operator overloading, macros, implicit conversions, shadowing or
  global state. Every binding states its type. Calls with two or more arguments name them,
  so swapped arguments cannot compile.
- **Checks built in.** Units of measure (`f64[m/s]`), effects (`uses io, fs, clock, random`), a
  checked promise not to allocate (`noalloc`), contracts (`requires`/`ensures`) and `test`
  blocks are part of the language, not libraries.
- **Errors as values.** `Option` and `Result` with `?`, and `match` that must handle every
  variant: there is no catch-all arm and no exception to forget.
- **Values, not references.** Arrays and structs copy on assignment (cheaply: storage is shared
  until written), so no two names ever see each other's changes.
- **Deterministic and loud.** Integer overflow panics. Random numbers come from one specified,
  seeded sequence, and tests read a virtual clock, so output is byte-identical on every run and
  platform. The same source gives a byte-identical executable.

The whole language is in [docs/SPEC.md](docs/SPEC.md), which is short enough to give an agent
in full (`anvil spec` prints it).

## Status

The language has programs of one or more files (modules with explicit imports and `pub`), generic
functions, structs and enums with built-in constraints, i64, f64 with units, bool, strings with interpolation,
search and split (`s = s + x` appends in place, so building text is linear), growable arrays and slices (a variable, field or element read for the last time moves, so `xs = f(xs)` and `s.items = f(s.items)` change it in place), maps in key order, structs, enums with
exhaustive `match`, structural `==`, `Option` and `Result` with `?`, function values and
closures, methods and interfaces, bytes and bit operations, deterministic parallel work
(`parallel_map`), built-ins for text,
sorting, stderr, exit codes, the environment and directories, a standard library written in
Anvil (`std.list`, `std.set`, `std.text`, `std.math`),
contracts (including `steps` and `allocs` bounds on a call's work and memory, checked by
verify), effects (io with
arguments and stdin, files, a clock, seeded random numbers),
`noalloc`, C functions (`extern`) and tests. There are two backends:

- an **interpreter**, which is the reference semantics;
- a **native compiler**: Cranelift to x86-64, linked with MSVC on Windows. Strings and arrays
  are reference counted with copy-on-write; the tests check that every object is freed.

The test suite runs every program through both and requires identical stdout, panic messages
and exit codes, on a corpus, on every test (`anvil test --native`) and on random programs
(`anvil fuzz`). Property tests (`test "name" for all xs: [i64] { ... }`) run on seeded,
generated inputs and shrink failures. Editors get a language server (`anvil lsp`) that answers
from the same commands. More platforms are on the [roadmap](docs/ROADMAP.md).

## Using it

You need Rust (stable). Native builds are for the machine you build on and need its linker:
the Visual Studio C++ build tools on Windows x64, a C compiler (`cc`, or `$CC`) on Linux and
macOS. A program with C functions (`extern "x.c" fn`) also compiles them with that compiler, for
the interpreter as well as for executables. Windows x64 and Linux x86-64 are tested; ARM64 Linux and macOS are built and tested by
the release workflow. `scripts/release.sh` packs the binaries of the host into `dist/`.

From crates.io, once it is published:

```bash
cargo install anvil-lang
```

From this repository:

```bash
cargo build --release
```

```bash
target/release/anvil run examples/orders.anv
```

```bash
target/release/anvil build examples/orders.anv -o orders.exe
```

| Command | Does |
|---|---|
| `anvil check f.anv` | type-check |
| `anvil run f.anv [--max-steps N]` | interpret `main` |
| `anvil build f.anv [-o out.exe] [--asm]` | compile to a native executable |
| `anvil test f.anv [--native] [--cases N]` | run `test` blocks, property tests on generated inputs |
| `anvil fuzz [--seed N] [--count N]` | random programs on both backends |
| `anvil fmt [f.anv] [--check]` | put a file, or every project file, in the canonical layout |
| `anvil fix f.anv` | apply every mechanical fix until none is left |
| `anvil outline f.anv` | list items and signatures |
| `anvil type-at f.anv LINE COL` | type and declaration at a position |
| `anvil refs f.anv NAME` | where a function, type, field, variant or unit is declared and used (or `LINE:COL`) |
| `anvil complete f.anv LINE COL` | what can be written at a position: fields and methods after `.`, names in scope elsewhere |
| `anvil lsp` | a language server on stdio, for editors |
| `anvil trace f.anv [--test NAME] [--only F,G]` | a run as a timeline: calls with their arguments, returns, prints |
| `anvil profile f.anv [--test NAME]` | calls, steps and bytes allocated per function |
| `anvil cost f.anv` | how each pure function's steps and bytes grow, and the `steps` and `allocs` bounds that fit |
| `anvil explain [CODE]` | explain an error code or a lint |
| `anvil spec` | print the language specification |
| `anvil std [MODULE]` | the standard library: modules and their pub items, or one module's source |
| `anvil docs [--out DIR]` | write a static site: the specification, every error code, the built-ins, the standard library |

Every command takes `--json`.

### Projects

A directory with an `anvil.toml` naming its files is a project. Project commands add approved
output for every program (goldens, checked on both backends), a project record of milestones,
decisions and findings with their history, and a review sheet for sign-off:

| Command | Does |
|---|---|
| `anvil list` | the project's files |
| `anvil golden check [--native]` | compare every program's output with its approved golden |
| `anvil golden approve f.anv` | approve a program's current output |
| `anvil api [f.anv]` | public APIs against their approved ones (`golden/f.anv.api`) |
| `anvil api approve f.anv` | approve a file's current public API |
| `anvil verify [--native]` | check, test and golden-check everything; findings go into the record |
| `anvil review` | verify, then write `out/review.md` for a person to sign off |
| `anvil mutate FILE` | mutation testing: change one operator, literal or call at a time; report each change no test or golden notices |
| `anvil watch` | verify, and again after every change |
| `anvil record query [KIND]` | milestones, decisions, findings or runs |

`verify` also applies two coverage lints: every function must run in a test or in a
golden-checked `main` (`untested`), and so must every branch of it: each `if` and `else` block,
match arm and loop body (`unreached`). Where that cannot be, `allow untested "why"` or
`allow unreached "why"` before the `fn` says so, and an allow that is no longer needed is
reported too. A module's public API is approved like a program's output: verify reports any
change to a `pub` signature, field, variant or method, and names the files a removal or change
breaks. Every `pub` item needs a doc comment (`undocumented`): the `//` lines above it,
which outline, completion, hover, `anvil std` and the docs site show with its signature; a doc
line `// example: EXPR` is also a test. A function's contract is checked without tests: verify calls every pure function
that has a `requires` or `ensures` on 100 generated arguments and reports, shrunk, any its
requires accept on which it panics or breaks its ensures (`contract`). `anvil mutate FILE` goes further: it changes the code one operator, literal or
call at a time and reports each change that no test or golden notices. A program that no longer matches its golden
leaves its actual output and a diff in `out/golden/`.

This repository is itself an Anvil project: `anvil golden check --native` runs its whole parity
corpus.

## In an editor

`anvil lsp` is a language server on stdio: errors as you type (with their fixes as quick fixes),
hover types, go to definition (into imported modules), references, completion, formatting and
an outline. Point any LSP client at the command `anvil lsp` for `*.anv` files; in Neovim:

```lua
vim.lsp.config("anvil", { cmd = { "anvil", "lsp" }, filetypes = { "anvil" }, root_markers = { "anvil.toml" } })
vim.lsp.enable("anvil")
```

Its answers are the commands' answers, so an editor, the CLI and an agent see the same thing.

## For agents: the MCP server

`anvil-mcp` serves every command above except `watch` and `lsp` as an MCP tool over stdio, with the same
arguments and the same JSON (a test keeps the two in step). A failing check is a tool error. Any MCP
client can run it. Most (Cursor, Gemini CLI, Claude Desktop and others) take it in their JSON settings:

```json
{ "mcpServers": { "anvil": { "command": "target/release/anvil-mcp", "args": ["--root", "."] } } }
```

and Claude Code adds it with:

```bash
claude mcp add anvil -- target/release/anvil-mcp --root .
```

An agent starts with `spec` (the whole language), writes a file, then loops on `check` and
`fix`; `outline`, `type_at`, `refs` and `complete` let it navigate a file without reading all of it, and
`trace` and `profile` show what a run did, counted in steps so the answer never varies.

Agents working on Anvil itself start from [AGENTS.md](AGENTS.md), the brief every coding agent reads;
CLAUDE.md points to it.

## Layout

| Path | Contents |
|---|---|
| `crates/anvil_front` (`anvil-lang-front`) | lexer, parser, checker, diagnostics, typed IR |
| `crates/anvil_interp` (`anvil-lang-interp`) | reference interpreter |
| `crates/anvil_codegen` (`anvil-lang-codegen`) | Cranelift backend, C runtime, linker driver |
| `crates/anvil_ops` (`anvil-lang-ops`) | every command, returning the JSON the CLI and MCP server send |
| `crates/anvil_cli` (`anvil-lang`) | the `anvil` command and end-to-end tests |
| `crates/anvil_mcp` (`anvil-lang-mcp`) | the `anvil-mcp` server, its stdio and parity tests |
| `crates/anvil_record` (`anvil-lang-record`) | the project record (SQLite) |
| `tests/programs` | programs whose approved output is in `golden/`, run on both backends; `lib/` holds the modules they import |
| `tests/errors` | one file per error code, with expected codes and fix checks |

Contributing: CONTRIBUTING.md. Changes by version: CHANGELOG.md.

## License

Copyright (c) 2026 Pixiliated.

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your
option. Contributions are accepted under the same terms.
